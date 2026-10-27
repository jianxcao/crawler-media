use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::path::PathBuf;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};

use super::{library_for_file, require_visible_library};

/// Queue metadata extraction and enabled TV fingerprint extraction.
fn enqueue_probe(state: &ApiState, row: &domain::FileCoverage) -> bool {
    matches!(
        state
            .probe
            .request_probe(row, ProbeRequestOrigin::Detail)
            .unwrap_or(ProbeRequestResult::Cancelled),
        ProbeRequestResult::Queued { .. }
    )
}

pub(crate) fn ensure_probe_enqueued(state: &ApiState, row: &domain::FileCoverage) -> bool {
    enqueue_probe(state, row)
}

/// Read back the committed coverage a writer just published.
///
/// A published file that the ledger no longer knows about is a recoverable
/// degradation, not a silent drop: log it and skip the enqueue.
fn committed_coverage(
    store: &crate::Store,
    path: &std::path::Path,
    media_id: Option<domain::MediaId>,
) -> Option<domain::FileCoverage> {
    let raw = path.display().to_string();
    match store.file_coverage_by_path(&raw) {
        Ok(Some(coverage)) => Some(coverage),
        Ok(None) => {
            tracing::warn!(
                path = %raw,
                media_id = ?media_id,
                "【媒体探测】已发布文件不在台账中，探测未入队"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                path = %raw,
                media_id = ?media_id,
                %error,
                "【媒体探测】读取台账失败，探测未入队"
            );
            None
        }
    }
}

/// Start metadata and voiceprint work for files a writer just published.
///
/// The caller hands over the published identity; the committed row is read back
/// from the Library so the probe policy sees the stored units and facts rather
/// than the writer's in-memory copy.
pub(crate) fn enqueue_probes_for_files(state: &ApiState, files: &[domain::LibraryFile]) -> usize {
    let rows: Vec<_> = {
        let store = state.store.lock();
        files
            .iter()
            .filter_map(|file| committed_coverage(&store, &file.path, Some(file.media_id)))
            .collect()
    };
    rows.iter().filter(|row| enqueue_probe(state, row)).count()
}

pub(crate) fn enqueue_probes_for_paths(
    state: &ApiState,
    paths: impl IntoIterator<Item = PathBuf>,
) -> usize {
    let rows: Vec<_> = {
        let store = state.store.lock();
        paths
            .into_iter()
            .filter_map(|path| committed_coverage(&store, &path, None))
            .collect()
    };
    rows.iter().filter(|row| enqueue_probe(state, row)).count()
}

/// POST /libraries/{id}/items/{item_id}/probe: clear the item's caches and re-probe.
pub(crate) async fn probe_item(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let (rows, media_kind) = {
        let store = state.store.lock();
        let library = match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        };
        let all_rows: Vec<domain::FileCoverage> = store
            .file_coverage_for_media(media_id)
            .unwrap_or_default()
            .into_iter()
            .collect();
        if !all_rows.iter().any(|row| {
            library_for_file(&store, &row.file, &media).is_some_and(|owner| owner.id == library.id)
        }) {
            return err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目没有在位文件",
            );
        }
        (all_rows, media.kind)
    };
    let total = rows.len();
    let units = rows
        .into_iter()
        .map(|row| crate::probe_manager::ProbeUnit {
            row,
            kind: media_kind,
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
            marker_refresh_id: None,
            job_id: None,
        })
        .collect();
    match state.probe.enqueue_forced_item_refresh(units) {
        Ok(queued) => {
            tracing::info!(media_id = %media_id, queued, total, "手动触发探测：已持久化整条目媒体信息与声纹任务");
            ok(json!({ "queued": queued, "already_running": false })).into_response()
        }
        Err(crate::probe_manager::ProbeEnqueueError::AlreadyRunning) => {
            ok(json!({ "queued": 0, "already_running": true })).into_response()
        }
        Err(crate::probe_manager::ProbeEnqueueError::Persistence) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "library.probe_enqueue_failed",
            "探测任务入队失败，请查看服务日志后重试",
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use domain::{Media, MediaId, MediaKind, QualitySource};

    use super::*;

    struct NoFetch;

    impl indexer::Fetcher for NoFetch {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("offline".into()))
        }
    }

    /// A store with one visible Movie library and the claimed file already
    /// published through the new writer, so the shim reads a committed row.
    fn fixture() -> (tempfile::TempDir, ApiState, domain::LibraryFile) {
        let dir = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            crate::Store::open(dir.path().join("data")).unwrap(),
            "test-token".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let dest = root.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        let file = {
            let store = state.store();
            let guard = store.lock();
            guard.insert_media(&media).unwrap();
            guard
                .create_library(
                    MediaKind::Movie,
                    "Movies",
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            drop(guard);
            crate::claim::publish_claimed_file(
                &store,
                &media,
                &dest,
                &crate::claim::ClaimedFacts::for_path(media.kind, &dest),
            )
            .unwrap()
        };
        assert_eq!(file.quality_source, QualitySource::Release);
        (dir, state, file)
    }

    #[test]
    fn enqueue_probes_for_files_queues_the_committed_row_behind_the_identity() {
        let (_dir, state, file) = fixture();

        let queued = enqueue_probes_for_files(&state, std::slice::from_ref(&file));

        assert_eq!(queued, 1);
        let store = state.store();
        let guard = store.lock();
        assert!(guard.is_probe_queued(&file.file_id.to_string()).unwrap());
    }

    #[test]
    fn enqueue_probes_for_files_ignores_an_identity_without_a_committed_row() {
        let (_dir, state, file) = fixture();
        let mut absent = file.clone();
        absent.path = file.path.with_file_name("Absent.1999.mkv");

        let queued = enqueue_probes_for_files(&state, std::slice::from_ref(&absent));

        assert_eq!(queued, 0);
        let store = state.store();
        let guard = store.lock();
        assert!(!guard.is_probe_queued(&absent.file_id.to_string()).unwrap());
    }
}
