//! Downloaders: qBittorrent / Transmission instances, live tasks, submit.

use std::collections::HashMap;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::http::{err, ok};
use crate::management::ApiState;

/// 种子已不在下载器时，pending 行的自动清理宽限期（秒）。
///
/// 提交后的短窗口内种子可能尚未在客户端可见（URL/磁力投递需要时间入库），
/// 宽限期内缺失只如实上报 missing 不删行；超过宽限期且客户端可达仍找不到，
/// 判定为「任务已消失」，自动清理 pending 行（下一轮搜索会按订阅重新投递）。
pub(super) const ABSENT_CLEANUP_GRACE_SECS: i64 = 15 * 60;

#[derive(Deserialize)]
pub(crate) struct SubmitInput {
    pub(super) download_url: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) site_id: Option<String>,
    #[serde(default)]
    pub(super) torrent_id: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) publish_time: Option<String>,
    #[serde(default)]
    pub(super) size_bytes: Option<u64>,
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    /// 投给某个订阅：写 pending + 记 grab，让 Transfer 能收集入库。
    #[serde(default)]
    pub(super) subscribe_id: Option<String>,
    /// Explicit downloader-side directory. For auto_route this is the host-side
    /// path returned by the routing preview and is translated through path_maps.
    #[serde(default)]
    pub(super) save_path: Option<String>,
    #[serde(default)]
    pub(super) auto_route: bool,
}

fn routed_task_client(
    state: &ApiState,
    query: &HashMap<String, String>,
) -> Result<std::sync::Arc<dyn downloader::Downloader>, Response> {
    let id = match query.get("downloader_id").filter(|id| !id.is_empty()) {
        Some(raw) => Some(domain::DownloaderId::from_str(raw).map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid_id",
                "下载器 ID 不是合法 UUID",
            )
        })?),
        None => None,
    };
    crate::delivery::client_for_id(state, id).map_err(|error| {
        err(
            StatusCode::BAD_GATEWAY,
            "downloader.error",
            &error.to_string(),
        )
    })
}

pub(super) fn task_state(raw: &str, progress: f64) -> &'static str {
    match raw {
        "error" => "error",
        "missingFiles" => "missing",
        "stalledDL" => "stalled",
        "downloading" | "forcedDL" | "metaDL" | "forcedMetaDL" | "allocating" => "downloading",
        "queuedDL" | "queuedUP" | "queuedForChecking" => "queued",
        "pausedDL" | "pausedUP" | "stoppedDL" | "stoppedUP" => "paused",
        "checkingDL" | "checkingUP" | "checkingResumeData" | "moving" => "checking",
        // Seeding states: the bytes are complete.
        "uploading" | "stalledUP" | "forcedUP" => "completed",
        _ if progress >= 1.0 => "completed",
        _ => "unknown",
    }
}

/// DELETE /downloaders/tasks/{hash}?delete_files=false
pub(crate) async fn delete_task(
    State(state): State<ApiState>,
    Path(hash): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let delete_files = query.get("delete_files").is_some_and(|v| v == "true");
    let downloader = match routed_task_client(&state, &query) {
        Ok(client) => client,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || downloader.delete_task(&hash, delete_files)).await {
        Ok(Ok(())) => ok(json!({"ok": true})).into_response(),
        Ok(Err(e)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &e.to_string(),
        ),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &e.to_string(),
        ),
    }
}

/// POST /downloaders/tasks/{hash}/pause
pub(crate) async fn pause_task(
    State(state): State<ApiState>,
    Path(hash): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let downloader = match routed_task_client(&state, &query) {
        Ok(client) => client,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || downloader.pause_task(&hash)).await {
        Ok(Ok(())) => ok(json!({"ok": true})).into_response(),
        Ok(Err(e)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &e.to_string(),
        ),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &e.to_string(),
        ),
    }
}

/// POST /downloaders/tasks/{hash}/resume
pub(crate) async fn resume_task(
    State(state): State<ApiState>,
    Path(hash): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let downloader = match routed_task_client(&state, &query) {
        Ok(client) => client,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || downloader.resume_task(&hash)).await {
        Ok(Ok(())) => ok(json!({"ok": true})).into_response(),
        Ok(Err(e)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &e.to_string(),
        ),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &e.to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Subscription-delivered tasks, addressed by the task-center `id`
// ---------------------------------------------------------------------------
//
// `list_tasks` builds each item's `id` as `{subscribe_id}:{enclosure}`. That is
// the only stable handle the task center has: qBittorrent info hashes are not
// known until the torrent is actually in the client, so `info_hash` is null for
// subscribe deliveries. These two endpoints therefore take the task id.

/// Split a task-center id into `(subscribe_id, enclosure)`.
///
/// Enclosures are URLs and therefore contain `:` — always split on the first
/// one so the enclosure survives intact.
fn parse_task_id(raw: &str) -> Option<(&str, &str)> {
    let (subscribe_id, enclosure) = raw.split_once(':')?;
    if subscribe_id.is_empty() || enclosure.is_empty() {
        return None;
    }
    Some((subscribe_id, enclosure))
}

#[derive(Deserialize)]
pub(crate) struct TaskInput {
    task_id: String,
    #[serde(default)]
    delete_files: bool,
}

/// Resolve a task id to its stored Torrent, or explain why it cannot be found.
fn resolve_task(
    store: &crate::Store,
    task_id: &str,
) -> Result<(domain::SubscribeId, crate::store::PendingDownload), String> {
    let (subscribe_id, enclosure) =
        parse_task_id(task_id).ok_or_else(|| "任务 id 无效".to_string())?;
    let subscribe_id =
        domain::SubscribeId::from_str(subscribe_id).map_err(|_| "任务 id 无效".to_string())?;
    let torrent = store
        .list_all_pending_routed()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|(id, _, pending)| *id == subscribe_id && pending.torrent.enclosure == enclosure)
        .map(|(_, _, pending)| pending)
        .ok_or_else(|| "任务不存在或已入库".to_string())?;
    Ok((subscribe_id, torrent))
}

/// POST /downloaders/tasks/remove — remove a subscription-delivered task.
///
/// Unlike `DELETE /downloaders/tasks/{hash}`, this works for tasks whose info
/// hash is unknown: the stored Torrent is matched inside the downloader by tag
/// or name, then the pending row is dropped.
pub(crate) async fn remove_task(
    State(state): State<ApiState>,
    Json(body): Json<TaskInput>,
) -> Response {
    let (subscribe_id, pending) = {
        let store = state.store.lock();
        match resolve_task(&store, &body.task_id) {
            Ok(found) => found,
            Err(message) => return err(StatusCode::NOT_FOUND, "task.missing", &message),
        }
    };
    let downloader = match crate::delivery::client_for_id(&state, pending.downloader_id) {
        Ok(client) => client,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "downloader.error",
                &error.to_string(),
            );
        }
    };
    let delete_files = body.delete_files;
    let torrent_for_remove = pending.torrent.clone();
    let remove_res = tokio::task::spawn_blocking(move || {
        downloader.remove_owned(&torrent_for_remove, delete_files)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|res| res.map_err(|e| e.to_string()));
    if let Err(error) = remove_res {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "downloader.error",
            &error,
        );
    }
    if let Err(error) = state
        .store
        .lock()
        .delete_pending(subscribe_id, &pending.torrent.enclosure)
    {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({
        "ok": true,
        "subscribe_id": subscribe_id.to_string(),
        "delete_files": delete_files,
    }))
    .into_response()
}

/// POST /downloaders/tasks/replace — search for an alternative source.
///
/// Queues an immediate one-shot `subscribe_search` for the owning Subscribe
/// with the stalled enclosure excluded, so this round can only select a
/// *different* source. The stalled task is deliberately left in place: it keeps
/// seeding/downloading until a replacement actually lands.
pub(crate) async fn replace_task(
    State(state): State<ApiState>,
    Json(body): Json<TaskInput>,
) -> Response {
    let (subscribe_id, pending) = {
        let store = state.store.lock();
        match resolve_task(&store, &body.task_id) {
            Ok(found) => found,
            Err(message) => return err(StatusCode::NOT_FOUND, "task.missing", &message),
        }
    };
    let payload = json!({
        "subscribe_id": subscribe_id.to_string(),
        "exclude_enclosure": pending.torrent.enclosure,
    })
    .to_string();
    let queue = state.jobs.lock();
    let job = match queue.enqueue_with_key(
        jobs::JobKind::SubscribeSearch,
        &payload,
        crate::job_loop::unix_now(),
        Some(&format!("subscribe:{subscribe_id}")),
    ) {
        Ok(job) => job,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "job.enqueue_failed",
                &error.to_string(),
            );
        }
    };
    tracing::info!(
        subscribe_id = %subscribe_id,
        excluded = %pending.torrent.enclosure,
        job_id = %job.id,
        "已为卡住的任务排队换源搜索"
    );
    ok(json!({
        "ok": true,
        "subscribe_id": subscribe_id.to_string(),
        "job_id": job.id.to_string(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::{parse_task_id, task_state};

    #[test]
    fn task_id_splits_on_the_first_colon_so_urls_survive() {
        let (subscribe, enclosure) =
            parse_task_id("11111111-1111-1111-1111-111111111111:https://pt.example/dl?id=1")
                .expect("valid task id");
        assert_eq!(subscribe, "11111111-1111-1111-1111-111111111111");
        assert_eq!(enclosure, "https://pt.example/dl?id=1");
    }

    #[test]
    fn task_id_rejects_missing_halves() {
        assert!(parse_task_id("no-colon").is_none());
        assert!(parse_task_id(":https://pt.example/dl").is_none());
        assert!(parse_task_id("sub-1:").is_none());
    }

    #[test]
    fn qbittorrent_states_map_onto_the_task_center_vocabulary() {
        assert_eq!(task_state("stalledDL", 0.4), "stalled");
        assert_eq!(task_state("error", 0.0), "error");
        assert_eq!(task_state("missingFiles", 0.0), "missing");
        assert_eq!(task_state("downloading", 0.4), "downloading");
        assert_eq!(task_state("forcedDL", 0.4), "downloading");
        assert_eq!(task_state("metaDL", 0.0), "downloading");
        assert_eq!(task_state("queuedDL", 0.0), "queued");
        assert_eq!(task_state("pausedDL", 0.4), "paused");
        assert_eq!(task_state("stoppedUP", 1.0), "paused");
        assert_eq!(task_state("checkingDL", 0.0), "checking");
        assert_eq!(task_state("uploading", 1.0), "completed");
        assert_eq!(task_state("stalledUP", 1.0), "completed");
        // Unknown strings with complete bytes are complete; otherwise unknown.
        assert_eq!(task_state("somethingNew", 1.0), "completed");
        assert_eq!(task_state("somethingNew", 0.3), "unknown");
    }
}
