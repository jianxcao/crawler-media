use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::path::PathBuf;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};

use super::{library_for_row, require_visible_library};

/// Queue metadata extraction and enabled TV fingerprint extraction.
fn enqueue_probe(state: &ApiState, row: &domain::LedgerRow) -> bool {
    matches!(
        state
            .probe
            .request_probe(row, ProbeRequestOrigin::Detail)
            .unwrap_or(ProbeRequestResult::Cancelled),
        ProbeRequestResult::Queued { .. }
    )
}

pub(crate) fn ensure_probe_enqueued(state: &ApiState, row: &domain::LedgerRow) -> bool {
    enqueue_probe(state, row)
}

/// Start metadata and voiceprint work as soon as new ledger rows enter a library.
pub(crate) fn enqueue_probes_for_rows(state: &ApiState, rows: &[domain::LedgerRow]) -> usize {
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
            .filter_map(|path| {
                store
                    .ledger_by_path(&path.display().to_string())
                    .ok()
                    .flatten()
            })
            .collect()
    };
    enqueue_probes_for_rows(state, &rows)
}

/// Return cached tracks immediately and arrange a background refresh on a miss.
pub(crate) async fn file_tracks(state: &ApiState, row: &domain::LedgerRow) -> library::Tracks {
    let key = row.id.to_string();
    let cached = state.store.lock().get_file_meta(&key).ok().flatten();
    let _ = ensure_probe_enqueued(state, row);
    cached.unwrap_or_default()
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
        let all_rows: Vec<domain::LedgerRow> = store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.media_id == media_id)
            .collect();
        if !all_rows.iter().any(|row| {
            library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
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
