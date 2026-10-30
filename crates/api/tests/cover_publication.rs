//! C2b：Library 封面版本化 CAS 发布。
//!
//! 渲染结果先落成新的不可变版本文件，DB 指针最后 CAS 发布；晚到的自动渲染
//! 不得覆盖更新的手动封面或 `none` 标记。
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use api::library_cover_service::{CoverRenderer, LibraryCoverService};
use api::{ApiState, Store, router};
use cover_generator::CoverOptions;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use store::test_support::{FixtureFile, publish_fixture_file};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

struct Gate {
    entered: std::sync::Mutex<bool>,
    entered_cv: std::sync::Condvar,
    open: std::sync::Mutex<bool>,
    open_cv: std::sync::Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: std::sync::Mutex::new(false),
            entered_cv: std::sync::Condvar::new(),
            open: std::sync::Mutex::new(false),
            open_cv: std::sync::Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let released = *self
            .open_cv
            .wait_timeout_while(
                self.open.lock().unwrap(),
                std::time::Duration::from_secs(30),
                |open| !*open,
            )
            .unwrap()
            .0;
        assert!(released, "test must release the cover gate");
    }

    fn entered(&self) -> bool {
        *self.entered.lock().unwrap()
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 卡住渲染，直到测试放行。
struct GatedRenderer {
    gate: Arc<Gate>,
    bytes: Vec<u8>,
}

impl CoverRenderer for GatedRenderer {
    fn render(
        &self,
        _library: &store::Library,
        _rows: &[domain::LibraryFile],
        _options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        self.gate.enter_and_wait();
        Ok(Some(self.bytes.clone()))
    }
}

/// 立即返回固定字节的渲染器（不阻塞）。
struct FixedRenderer(Vec<u8>);

impl CoverRenderer for FixedRenderer {
    fn render(
        &self,
        _library: &store::Library,
        _rows: &[domain::LibraryFile],
        _options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(Some(self.0.clone()))
    }
}

async fn wait_entered(gate: &Arc<Gate>) {
    let entered = tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(entered.is_ok(), "cover renderer was never invoked");
}

/// 测试 panic 时必须放行渲染线程：渲染跑在 tokio 的 blocking pool 上，若它仍卡在
/// `Gate::enter_and_wait`，runtime 关闭 blocking pool 会无限等待，测试进程永不退出。
struct ReleaseOnDrop<'a>(&'a Arc<Gate>);

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn seed_library(state: &ApiState, root: &Path) -> String {
    let library_root = root.join("covers-library");
    std::fs::create_dir_all(&library_root).unwrap();
    let file = library_root.join("Movie.mkv");
    std::fs::write(&file, b"video").unwrap();
    std::fs::write(library_root.join("poster.jpg"), b"poster").unwrap();
    let store = state.store();
    let guard = store.as_ref();
    let library = guard
        .create_library(
            MediaKind::Movie,
            "Cover Library",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Cover Movie".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media).unwrap();
    publish_fixture_file(
        guard,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: file.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    library.id
}

fn cover_path(state: &ApiState, library_id: &str) -> Option<String> {
    let store = state.store();
    let guard = store.as_ref();
    guard.get_library(library_id).unwrap().unwrap().cover_path
}

fn referenced_bytes(state: &ApiState, library_id: &str) -> Option<Vec<u8>> {
    let path = cover_path(state, library_id)?;
    std::fs::read(path).ok()
}

fn cover_dir_files(state: &ApiState) -> Vec<PathBuf> {
    let dir = {
        let store = state.store();
        let guard = store.as_ref();
        guard.library_covers_dir()
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
}

fn gated_state(root: &Path, rendered: &[u8]) -> (ApiState, Arc<Gate>) {
    let gate = Arc::new(Gate::new());
    let service = LibraryCoverService::new(Arc::new(GatedRenderer {
        gate: gate.clone(),
        bytes: rendered.to_vec(),
    }));
    let state = make_state(root).with_cover_service(Arc::new(service));
    (state, gate)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_auto_render_does_not_overwrite_manual_cover() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, gate) = gated_state(tmp.path(), b"auto-rendered-cover");
    let _release = ReleaseOnDrop(&gate);
    let library_id = seed_library(&state, tmp.path());

    let auto_state = state.clone();
    let auto_library = library_id.clone();
    let auto = tokio::spawn(async move {
        auto_state
            .cover_service()
            .ensure_cover(&auto_state, &auto_library)
            .await
    });
    wait_entered(&gate).await;

    // 渲染被卡住期间，用户手动上传了更新的封面。
    let manual = b"manual-cover-bytes".to_vec();
    let manual_path = state
        .cover_service()
        .replace_cover(&state, &library_id, manual.clone())
        .await
        .unwrap();
    gate.release();

    let auto_result = auto.await.unwrap();
    assert!(
        auto_result.is_err(),
        "late auto render must fail its revision CAS, got {auto_result:?}"
    );
    assert_eq!(
        PathBuf::from(cover_path(&state, &library_id).unwrap()),
        manual_path
    );
    assert_eq!(referenced_bytes(&state, &library_id).unwrap(), manual);
    assert!(
        !cover_dir_files(&state).iter().any(|path| path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("auto"))),
        "unreferenced auto version must be discarded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_cover_is_not_republished_by_late_render() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, gate) = gated_state(tmp.path(), b"auto-rendered-cover");
    let _release = ReleaseOnDrop(&gate);
    let library_id = seed_library(&state, tmp.path());

    let auto_state = state.clone();
    let auto_library = library_id.clone();
    let auto = tokio::spawn(async move {
        auto_state
            .cover_service()
            .ensure_cover(&auto_state, &auto_library)
            .await
    });
    wait_entered(&gate).await;

    state
        .cover_service()
        .remove_cover(&state, &library_id)
        .await
        .unwrap();
    gate.release();

    assert!(auto.await.unwrap().is_err());
    assert_eq!(cover_path(&state, &library_id).as_deref(), Some("none"));
    assert!(referenced_bytes(&state, &library_id).is_none());

    // 显式删除后，新的 ensure_cover 也不得回填。
    assert!(
        state
            .cover_service()
            .ensure_cover(&state, &library_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_library_does_not_publish_late_render() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, gate) = gated_state(tmp.path(), b"auto-rendered-cover");
    let _release = ReleaseOnDrop(&gate);
    let library_id = seed_library(&state, tmp.path());

    let auto_state = state.clone();
    let auto_library = library_id.clone();
    let auto = tokio::spawn(async move {
        auto_state
            .cover_service()
            .ensure_cover(&auto_state, &auto_library)
            .await
    });
    wait_entered(&gate).await;

    api::library_lifecycle::start_library_removal(&state, &library_id).unwrap();
    gate.release();

    assert!(
        auto.await.unwrap().is_err(),
        "frozen library must not publish a new cover revision"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn db_failure_keeps_previous_cover_and_drops_new_version() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let library_id = seed_library(&state, tmp.path());

    let first = b"first-manual-cover".to_vec();
    let first_path = state
        .cover_service()
        .replace_cover(&state, &library_id, first.clone())
        .await
        .unwrap();
    let before = cover_dir_files(&state);

    // DB 写失败：cover_path 更新被 trigger 拒绝。`libraries` 表在 library.db，
    // trigger 必须建在同一个连接能看到的库里才会生效。
    let db_path = {
        let store = state.store();
        let guard = store.as_ref();
        guard.data_dir().join("library.db")
    };
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_cover_update BEFORE UPDATE OF cover_path ON libraries
             BEGIN SELECT RAISE(ABORT, 'cover db down'); END;",
        )
        .unwrap();

    let error = state
        .cover_service()
        .replace_cover(&state, &library_id, b"second-cover".to_vec())
        .await
        .expect_err("DB failure must surface as an error");
    assert!(!error.is_empty());
    assert_eq!(
        PathBuf::from(cover_path(&state, &library_id).unwrap()),
        first_path
    );
    assert_eq!(referenced_bytes(&state, &library_id).unwrap(), first);
    assert_eq!(
        cover_dir_files(&state),
        before,
        "unreferenced new version must be deleted after a DB failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_only_does_not_write_files_or_db() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path()).with_cover_service(Arc::new(LibraryCoverService::new(
        Arc::new(FixedRenderer(b"preview-cover-bytes".to_vec())),
    )));
    let library_id = seed_library(&state, tmp.path());
    let before = cover_dir_files(&state);
    let revision_before = {
        let store = state.store();
        let guard = store.as_ref();
        guard.library_cover_revision(&library_id).unwrap()
    };

    let generated = state
        .cover_service()
        .generate(&state, &library_id, cover_options("Preview"), true)
        .await
        .unwrap();
    assert!(!generated.bytes.is_empty());
    assert!(generated.published_path.is_none());
    assert_eq!(cover_dir_files(&state), before);
    assert_eq!(cover_path(&state, &library_id), None);
    let revision_after = {
        let store = state.store();
        let guard = store.as_ref();
        guard.library_cover_revision(&library_id).unwrap()
    };
    assert_eq!(revision_before, revision_after);
}

fn cover_options(title: &str) -> CoverOptions {
    CoverOptions {
        title_zh: title.into(),
        title_en: None,
        width: 1920,
        height: 1080,
        style: cover_generator::CoverStyle::MultiPosterPile,
        background: cover_generator::BackgroundOption::default(),
    }
}

// router 只是为了复用 api 的公开导出，避免测试只依赖内部结构。
#[allow(dead_code)]
fn _router(state: &ApiState) -> axum::Router {
    router(state.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cover_get_serves_cached_bytes_while_writer_is_busy() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let state = make_state(tmp.path());
    let library = seed_library(&state, tmp.path());
    state
        .cover_service()
        .replace_cover(&state, &library, b"cached cover".to_vec())
        .await
        .unwrap();
    let app = router(state.clone());
    let handle = state.store();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(handle.clone(), store::Db::Library);
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    entered_rx.await.unwrap();
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        app.oneshot(
            Request::get(format!("/api/v1/libraries/{library}/cover"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let response = response
        .expect("a cached native image must never wait for the writer")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()["etag"].clone();
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
        b"cached cover".as_slice()
    );
    let response = router(state)
        .oneshot(
            Request::get(format!("/api/v1/libraries/{library}/cover"))
                .header("if-none-match", etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn cover_get_serves_default_while_background_render_is_running() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let (state, gate) = gated_state(tmp.path(), b"background cover");
    let _release = ReleaseOnDrop(&gate);
    let library = seed_library(&state, tmp.path());
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        router(state.clone()).oneshot(
            Request::get(format!("/api/v1/libraries/{library}/cover"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("GET must return before the renderer finishes")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        bytes.starts_with(b"\xff\xd8\xff"),
        "GET immediately serves the built-in default"
    );
    wait_entered(&gate).await;
    assert_eq!(cover_path(&state, &library), None);
    gate.release();
    tokio::time::timeout(Duration::from_secs(3), async {
        while cover_path(&state, &library).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        referenced_bytes(&state, &library).unwrap(),
        b"background cover"
    );
}

#[tokio::test]
async fn all_default_covers_are_refreshed_after_the_background_renderer_is_released() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let (state, gate) = gated_state(tmp.path(), b"queued cover");
    let _release = ReleaseOnDrop(&gate);
    let first = seed_library(&state, &tmp.path().join("first"));
    let second = seed_library(&state, &tmp.path().join("second"));
    for library in [&first, &second] {
        let response = router(state.clone())
            .oneshot(
                Request::get(format!("/api/v1/libraries/{library}/cover"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    wait_entered(&gate).await;
    gate.release();
    tokio::time::timeout(Duration::from_secs(2), async {
        while cover_path(&state, &first).is_none() || cover_path(&state, &second).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every admitted default must be refreshed without another GET");
}

#[tokio::test]
async fn missing_cached_cover_is_regenerated_in_background() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let state = make_state(tmp.path()).with_cover_service(Arc::new(LibraryCoverService::new(
        Arc::new(FixedRenderer(b"regenerated cover".to_vec())),
    )));
    let library = seed_library(&state, tmp.path());
    let path = state
        .cover_service()
        .replace_cover(&state, &library, b"removed cover".to_vec())
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let response = router(state.clone())
        .oneshot(
            Request::get(format!("/api/v1/libraries/{library}/cover"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(2), async {
        while cover_path(&state, &library).as_deref() == path.to_str() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a missing cache file must be rebuilt without blocking GET");
    assert_eq!(
        referenced_bytes(&state, &library).unwrap(),
        b"regenerated cover"
    );
}
