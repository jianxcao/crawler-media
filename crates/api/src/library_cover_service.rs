//! Library 封面渲染与版本化发布（C2b）。
//!
//! 渲染结果先写成新的不可变版本文件 `library-{id}-{uuid}.jpg`，**不覆盖**当前
//! 文件；DB 指针通过 `Store::compare_set_library_cover` 的 revision CAS 最后发布。
//! CAS 未命中或 DB 写失败时删除这个尚未被引用的新版本并返回错误。
//! `preview_only` 只返回字节，不写文件、不写 DB、不动 revision。

use std::path::{Path, PathBuf};
use std::sync::Arc;

mod refresh;

use cover_generator::CoverOptions;

use crate::ApiState;

/// Values of the `library_cover_checked.{library_id}` setting.
///
/// `MARKER_CHECKED` records that an automatic cover was published. `MARKER_RETRY`
/// marks an attempt worth retrying, and is the only value a scan clears (see
/// `clear_library_cover_checked`); clearing `checked` on every scan would
/// re-render covers needlessly.
pub const MARKER_CHECKED: &str = "checked";
pub const MARKER_MANUAL: &str = "manual";
pub const MARKER_RETRY: &str = "auto";

/// 锁内取到的 owned 事实快照；渲染/写盘全部在锁外进行。
pub struct CoverSnapshot {
    pub library: crate::store::Library,
    pub files: Vec<domain::LibraryFile>,
    pub revision: i64,
    pub marker: Option<String>,
    pub target_dir: PathBuf,
}

pub trait CoverRenderer: Send + Sync {
    fn render(
        &self,
        library: &crate::store::Library,
        rows: &[domain::LibraryFile],
        options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String>;
}

/// 生产渲染器：读取本地 poster.jpg 并用 cover-generator 合成。
pub struct LocalCoverRenderer;

impl CoverRenderer for LocalCoverRenderer {
    fn render(
        &self,
        library: &crate::store::Library,
        rows: &[domain::LibraryFile],
        options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        crate::http::library::render_cover_jpeg(library, rows, options)
    }
}

pub struct GeneratedCover {
    pub bytes: Vec<u8>,
    pub published_path: Option<PathBuf>,
}

pub struct LibraryCoverService {
    refresh: refresh::RefreshQueue,
    renderer: Arc<dyn CoverRenderer>,
}

impl LibraryCoverService {
    pub fn new(renderer: Arc<dyn CoverRenderer>) -> Self {
        Self {
            renderer,
            refresh: refresh::RefreshQueue::default(),
        }
    }

    /// 生产默认服务（真实渲染器）。
    pub fn local() -> Self {
        Self::new(Arc::new(LocalCoverRenderer))
    }

    pub async fn ensure_cover(
        &self,
        state: &ApiState,
        id: &str,
    ) -> Result<Option<PathBuf>, String> {
        let (lease, snapshot) = prepare(state, id).await?;
        if matches!(
            snapshot.library.cover_path.as_deref(),
            Some("none") | Some("")
        ) {
            return Ok(None);
        }
        if let Some(path) = existing_off_executor(state, snapshot.clone(), lease.clone()).await? {
            return Ok(Some(path));
        }
        if snapshot.marker.as_deref() == Some(MARKER_RETRY)
            || state.require_writable().is_err()
            || state.require_write_admitted().is_err()
        {
            return Ok(None);
        }
        let options = crate::http::library::default_cover_options(&snapshot.library);
        let bytes = render_off_lock(
            state,
            self.renderer.clone(),
            snapshot.clone(),
            options,
            lease.clone(),
        )
        .await?;
        let Some(bytes) = bytes else {
            mark_retry(state, snapshot).await;
            return Ok(None);
        };
        publish(state, snapshot, bytes, MARKER_CHECKED, lease)
            .await
            .map(Some)
    }

    pub async fn replace_cover(
        &self,
        state: &ApiState,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<PathBuf, String> {
        let (lease, snapshot) = prepare(state, id).await?;
        publish(state, snapshot, bytes, MARKER_MANUAL, lease).await
    }

    pub async fn generate(
        &self,
        state: &ApiState,
        id: &str,
        options: CoverOptions,
        preview_only: bool,
    ) -> Result<GeneratedCover, String> {
        let (lease, snapshot) = prepare(state, id).await?;
        let bytes = render_off_lock(
            state,
            self.renderer.clone(),
            snapshot.clone(),
            options,
            lease.clone(),
        )
        .await?
        .ok_or_else(|| "cover.empty: 媒体库内无可用海报图片".to_string())?;
        if preview_only {
            return Ok(GeneratedCover {
                bytes,
                published_path: None,
            });
        }
        let target = publish(state, snapshot, bytes.clone(), MARKER_MANUAL, lease).await?;
        Ok(GeneratedCover {
            bytes,
            published_path: Some(target),
        })
    }

    pub async fn remove_cover(&self, state: &ApiState, id: &str) -> Result<(), String> {
        let (lease, snapshot) = prepare(state, id).await?;
        let worker = state.clone();
        state
            .fs_work
            .reserve(WORK_WAIT)
            .await
            .map_err(work_error)?
            .run_to_completion(move || {
                let previous = existing_cover(&snapshot);
                let marker = snapshot.marker.as_deref().unwrap_or(MARKER_CHECKED);
                let published = cas_publish(&worker, &snapshot, Some("none"), marker, &lease)?;
                if !published {
                    return Err("cover.stale: 封面已被更新，请重试".into());
                }
                if let Some(path) = previous
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(&format!("library-{}-", snapshot.library.id))
                        })
                {
                    discard_unreferenced(&path);
                }
                Ok(())
            })
            .await
            .map_err(work_error)?
    }
}

const WORK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
type CoverLease = Arc<crate::library_lifecycle::LibraryWorkLease>;

fn work_error(error: crate::blocking_budget::BlockingError) -> String {
    match error {
        crate::blocking_budget::BlockingError::Busy
        | crate::blocking_budget::BlockingError::Timeout => "runtime.busy".into(),
        error => format!("cover.worker: {error}"),
    }
}

async fn prepare(state: &ApiState, id: &str) -> Result<(CoverLease, Arc<CoverSnapshot>), String> {
    let worker = state.clone();
    let id = id.to_string();
    state
        .http_work
        .reserve(WORK_WAIT)
        .await
        .map_err(work_error)?
        .run_to_completion(move || {
            let lease = worker
                .library_work()
                .acquire_bounded(&worker.store, &id)
                .map_err(|error| format!("cover.unavailable: {error}"))?;
            let store = worker.store.clone();
            Ok((Arc::new(lease), Arc::new(snapshot_from_store(&store, &id)?)))
        })
        .await
        .map_err(work_error)?
}

async fn existing_off_executor(
    state: &ApiState,
    snapshot: Arc<CoverSnapshot>,
    lease: CoverLease,
) -> Result<Option<PathBuf>, String> {
    state
        .fs_work
        .reserve(WORK_WAIT)
        .await
        .map_err(work_error)?
        .run_to_completion(move || {
            let _lease = lease;
            existing_cover(&snapshot)
        })
        .await
        .map_err(work_error)
}

/// 锁内只取 owned DB 事实。
pub fn snapshot(state: &ApiState, id: &str) -> Result<CoverSnapshot, String> {
    let store = state.store.clone();
    snapshot_from_store(&store, id)
}

fn snapshot_from_store(store: &crate::Store, id: &str) -> Result<CoverSnapshot, String> {
    let library = store
        .get_library(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "library.missing: 媒体库不存在".to_string())?;
    let files = crate::http::library::files_in_library(&store, &library);
    let marker = store
        .get_setting(&format!("library_cover_checked.{}", library.id))
        .ok()
        .flatten();
    Ok(CoverSnapshot {
        revision: library.cover_revision,
        marker,
        target_dir: store.library_covers_dir(),
        library,
        files,
    })
}

/// 当前已存在的封面（只读）：`none`/空串表示显式删除。
pub fn existing_cover(snapshot: &CoverSnapshot) -> Option<PathBuf> {
    match snapshot.library.cover_path.as_deref() {
        Some("none") | Some("") => return None,
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        None => {}
    }
    None
}

/// The publication gate contains bounded SQL only. Filesystem writes run before it.
fn cas_publish(
    state: &ApiState,
    snapshot: &CoverSnapshot,
    target: Option<&str>,
    marker: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<bool, String> {
    lease.publish(|| {
        crate::http::store_wait::with_foreground_store(state, |store| {
            state
                .require_writable()
                .map_err(|_| "instance.read_only".to_string())?;
            state
                .require_write_admitted()
                .map_err(|_| "runtime.busy".to_string())?;
            store
                .compare_set_library_cover(&snapshot.library.id, snapshot.revision, target, marker)
                .map_err(|error| error.to_string())
        })
        .map_err(|response| format!("cover write rejected ({})", response.status()))?
    })
}

async fn render_off_lock(
    state: &ApiState,
    renderer: Arc<dyn CoverRenderer>,
    snapshot: Arc<CoverSnapshot>,
    options: CoverOptions,
    lease: CoverLease,
) -> Result<Option<Vec<u8>>, String> {
    state
        .fs_work
        .reserve(WORK_WAIT)
        .await
        .map_err(work_error)?
        .run_to_completion(move || {
            let _lease = lease;
            renderer.render(&snapshot.library, &snapshot.files, &options)
        })
        .await
        .map_err(work_error)?
}

/// The worker retains the Library lease until bytes, CAS and failed-CAS cleanup
/// finish, even when the request future is cancelled while it is running.
async fn publish(
    state: &ApiState,
    snapshot: Arc<CoverSnapshot>,
    bytes: Vec<u8>,
    marker: &'static str,
    lease: CoverLease,
) -> Result<PathBuf, String> {
    let worker = state.clone();
    state.fs_work.reserve(WORK_WAIT).await.map_err(work_error)?.run_to_completion(move || {
        let target = snapshot.target_dir.join(format!("library-{}-{}.jpg", snapshot.library.id, uuid::Uuid::new_v4()));
        worker.require_writable().map_err(|_| "instance.read_only".to_string())?;
        worker.require_write_admitted().map_err(|_| "runtime.busy".to_string())?;
        std::fs::create_dir_all(&snapshot.target_dir).map_err(|error| format!("cover.write: {error}"))?;
        std::fs::write(&target, bytes).map_err(|error| format!("cover.write: {error}"))?;
        match cas_publish(&worker, &snapshot, Some(&target.display().to_string()), marker, &lease) {
            Ok(true) => {
                tracing::info!(library_id=%snapshot.library.id, cover_path=%target.display(), revision=snapshot.revision+1,
                    "published library cover revision");
                Ok(target)
            }
            Ok(false) => {
                discard_unreferenced(&target);
                Err("cover.stale: 封面已被更新的修改覆盖，已丢弃本次结果".into())
            }
            Err(error) => { discard_unreferenced(&target); Err(error) }
        }
    }).await.map_err(work_error)?
}

fn discard_unreferenced(target: &Path) {
    if let Err(error) = std::fs::remove_file(target)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path=%target.display(), "failed to discard unreferenced cover version");
    }
}

async fn mark_retry(state: &ApiState, snapshot: Arc<CoverSnapshot>) {
    let id = snapshot.library.id.clone();
    let logged_id = id.clone();
    let result = crate::http::store_wait::store_write(state, move |store| {
        if store.library_cover_revision(&id)? == Some(snapshot.revision)
            && store.library_lifecycle_state(&id)? == "ready"
        {
            store.put_setting(&format!("library_cover_checked.{id}"), MARKER_RETRY)?;
        }
        Ok::<_, crate::store::StoreError>(())
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(%error, library_id=%logged_id, "failed to mark library cover retry")
        }
        Err(response) => {
            tracing::warn!(status=%response.status(), library_id=%logged_id, "library cover retry marker was not admitted")
        }
    }
}

/// Remove every on-disk cover artifact owned by `library_id`.
///
/// Returns the number of versioned cover files removed.
pub fn remove_library_cover_files(covers_dir: &Path, library_id: &str) -> usize {
    let versioned = format!("library-{library_id}-");
    let entries = match std::fs::read_dir(covers_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(error) => {
            tracing::warn!(%error, dir = %covers_dir.display(), "读取封面目录失败，跳过封面清理");
            return 0;
        }
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(&versioned) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, path = %entry.path().display(), "删除媒体库封面文件失败");
            }
        }
    }
    if removed > 0 {
        tracing::info!(library_id, removed, "已清理媒体库封面文件");
    }
    removed
}

/// Reconcile covers of Libraries whose removal already succeeded.
///
/// A crash between the DB commit and the file unlink would otherwise leave the
/// bytes behind forever; every startup retries, so the cleanup is idempotent.
pub fn purge_removed_library_covers(state: &ApiState) {
    let (covers_dir, removals) = {
        let store = state.store.clone();
        (
            store.library_covers_dir(),
            store.succeeded_library_removals(),
        )
    };
    let removals = match removals {
        Ok(removals) => removals,
        Err(error) => {
            tracing::warn!(%error, "读取已完成的媒体库删除记录失败，跳过封面清理");
            return;
        }
    };
    for removal in removals {
        remove_library_cover_files(&covers_dir, &removal.library_id);
    }
}
