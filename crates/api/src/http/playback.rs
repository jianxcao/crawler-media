//! Playback views: player-facing decide/session/progress/metrics/policy,
//! resume/marks, and item/episode info. Live activity, history/stats and
//! up-next/favorites live in sibling modules; the Jellyfin-compatible routes
//! stay in `crates/api/src/jellyfin.rs`.
//! Item artwork is observed from an owned visibility snapshot outside Store,
//! under the shared HTTP blocking budget so slow filesystems cannot hold it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::str::FromStr;

use super::store_wait::store_read;
use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::UnitState;

mod chapter_cache;
mod direct;
use direct::direct_decision;
mod episode_listing;
pub(crate) use episode_listing::episodes;
pub(crate) mod episodes;
mod heartbeat;
mod heartbeat_input;
mod request;
mod selection;
mod session;
pub(crate) use heartbeat::progress;
use selection::row_for;
pub(crate) use session::{session_stream, start_session};
mod watch_state;
pub(crate) use chapter_cache::cached_chapters;
pub(crate) use watch_state::{marks, resume, set_marks};

const SESSION_TIMEOUT_SECS: i64 = 300;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn resolve_media_id(raw: &str) -> Option<domain::MediaId> {
    domain::MediaId::from_str(raw).ok()
}

/// The watch state of one target, `played` resolved through the assertion hierarchy.
///
/// A Store failure is an error, not an empty state: answering `played: false` for a
/// read that failed would tell the client the opposite of what the user marked.
pub(super) fn effective_watch_json(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Result<Value, crate::store::StoreError> {
    match effective_watch_state(store, user_id, media_id, season, episode)? {
        Some(unit) => Ok(unit_json(&unit)),
        // No assertion and no row on this unit; the raw row is the only remaining
        // source of progress/favourite facts.
        None => unit_for_unit(store, user_id, media_id, season, episode),
    }
}

pub(crate) fn effective_watch_state(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Result<Option<crate::store::UnitState>, crate::store::StoreError> {
    let Some((season, episode)) = selection::watch_unit(store, media_id, season, episode) else {
        return Ok(None);
    };
    store.effective_unit_state(user_id, media_id, domain::UnitKey::new(season, episode))
}

/// Library attribution for an item: the library whose roots contain an owned
/// file, falling back to the kind's default library.
pub(crate) fn library_id_for(
    store: &crate::Store,
    media: &domain::Media,
    files: &[&domain::LibraryFile],
) -> Option<String> {
    let mut ids = files
        .iter()
        .filter_map(|file| crate::http::library::library_for_file(store, file, media))
        .map(|library| library.id);
    let first = ids.next()?;
    ids.all(|id| id == first).then_some(first)
}

fn visible_coverages_for_media<'a>(
    store: &crate::Store,
    media: &domain::Media,
    coverages: &'a [domain::FileCoverage],
    user_id: Option<domain::UserId>,
) -> Vec<&'a domain::FileCoverage> {
    coverages
        .iter()
        .filter(|coverage| coverage.file.media_id == media.id)
        .filter(|coverage| {
            crate::http::library::file_visible_to_user(store, &coverage.file, media, user_id)
        })
        .collect()
}

/// The activity view's media target block for a session/log row.
///
/// 薄 wrapper：一次物化 `PlaybackRead` 后交给纯函数渲染；调用点签名保持不变。
pub(crate) fn media_target_json(
    store: &crate::Store,
    media_id: domain::MediaId,
    season: Option<i32>,
    episode: Option<i32>,
) -> Value {
    match crate::playback_read::PlaybackRead::load(store, &[media_id], &[]) {
        Ok(read) => crate::playback_read::media_target(&read, media_id, season, episode),
        Err(error) => {
            tracing::error!(%error, %media_id, "读取播放目标失败");
            json!({ "media_item_id": media_id.to_string(), "title": "", "kind": "movie" })
        }
    }
}

pub(crate) fn unit_json(unit: &UnitState) -> Value {
    json!({
        "position_ms": unit.position_ms,
        "played": unit.played,
        "play_count": unit.play_count,
        "duration_ms": unit.duration_ms,
        "audio_track": unit.audio_track,
        "subtitle_track": unit.subtitle_track,
        "is_favorite": unit.favorite,
    })
}

/// The watch-state response the player merges into its timeline.
pub(crate) fn watch_state_json(unit: &UnitState, ended_by_admin: bool) -> Value {
    let mut value = unit_json(unit);
    value["ended_by_admin"] = json!(ended_by_admin);
    value
}

// ---------------------------------------------------------------------------
// Item info + decide/session
// ---------------------------------------------------------------------------

pub(crate) async fn item(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    // Review L2: the item's ledger facts are read off the executor; a contended
    // lock is answered with "busy, retry" instead of parking a worker.
    let (media, library_id, preferred_coverage) = match store_read(&state, move |store| {
        let Some(media_id) = resolve_media_id(&id) else {
            return Err(err(
                StatusCode::NOT_FOUND,
                "playback.item_missing",
                "条目不存在",
            ));
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return Err(err(
                StatusCode::NOT_FOUND,
                "playback.item_missing",
                "条目不存在",
            ));
        };
        let all_coverages = store.file_coverage_for_media(media.id).unwrap_or_default();
        let coverages = visible_coverages_for_media(store, &media, &all_coverages, user_id);
        if coverages.is_empty() {
            return Err(err(
                StatusCode::NOT_FOUND,
                "playback.item_missing",
                "条目不存在",
            ));
        }
        let files: Vec<&domain::LibraryFile> =
            coverages.iter().map(|coverage| &coverage.file).collect();
        let library_id = library_id_for(store, &media, &files).unwrap_or_default();
        let preferred_coverage = crate::http::library::preferred_coverage(&coverages).cloned();
        Ok((media, library_id, preferred_coverage))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let files = state.file_stats.clone();
    let poster_url = match state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            preferred_coverage
                .as_ref()
                .filter(|coverage| {
                    crate::http::library::poster_path_with(&coverage.file, files.as_ref()).is_some()
                })
                .map(|coverage| crate::http::library::artwork_url("posters", coverage.file.file_id))
        })
        .await
    {
        Ok(url) => url,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            );
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return err(
                StatusCode::GATEWAY_TIMEOUT,
                "playback.timeout",
                "加载条目超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(message)) => {
            tracing::error!(%message, media_id = %media.id, "读取播放条目海报失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "加载条目失败",
            );
        }
    };
    ok(json!({
        "media_item_id": media.id.to_string(),
        "library_id": library_id,
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "poster_url": poster_url,
    }))
    .into_response()
}

/// POST /playback/decide — tier-0 direct decision only (no transcode).
pub(crate) async fn decide(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Json(body): Json<Value>,
) -> Response {
    // Review L2: the tier-0 decision reads the ledger; take the Store lock off the
    // executor and answer "busy, retry" instead of parking a worker on a batch.
    match store_read(&state, move |store| -> Response {
        let (file, _media, _, _) = match row_for(store, &body, user_id) {
            Ok(row) => row,
            Err(error) => return error.response(),
        };
        let tracks = store
            .get_file_meta(&file.file_id.to_string())
            .ok()
            .flatten();
        ok(direct_decision(&file, &body, tracks.as_ref())).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

pub(super) fn unit_for_unit(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Result<Value, crate::store::StoreError> {
    Ok(match store.unit_state(user_id, media_id, season, episode)? {
        Some(unit) => unit_json(&unit),
        None => json!({
            "position_ms": 0, "played": false, "play_count": 0, "duration_ms": null,
            "audio_track": null, "subtitle_track": null, "is_favorite": false,
        }),
    })
}

// ---------------------------------------------------------------------------
// Progress / metrics / policy / client-log
// ---------------------------------------------------------------------------

/// POST /playback/metrics — best-effort quality snapshot (never fails).
pub(crate) async fn metrics(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    // `playback_clock` 是毫秒时钟；`playback_metrics.recorded_at` 存 unix 秒。
    let now = state.playback_clock.now_ms() / 1000;
    // Review L2: metric persistence is a Store write; keep it off the executor.
    let result = store_read(&state, move |store| -> Response {
        let media_id = metric_media_id(store, user_id, &body);
        match media_id {
            Some(media_id) => {
                if let Err(error) = store.insert_metric(
                    user_id,
                    media_id,
                    body["tier"].as_i64().unwrap_or(0) as i32,
                    body["engine"].as_str(),
                    body["ttff_ms"].as_i64(),
                    body["rebuffer_ms"].as_i64().unwrap_or(0),
                    body["rebuffer_count"].as_i64().unwrap_or(0),
                    body["seek_count"].as_i64().unwrap_or(0),
                    body["dropped_frames"].as_i64(),
                    body["total_frames"].as_i64(),
                    body["watched_ms"].as_i64().unwrap_or(0),
                    now,
                ) {
                    tracing::warn!(%error, %user_id, %media_id, "Playback quality metric persistence failed");
                }
            }
            None => tracing::warn!(%user_id, library_file_id = ?body.get("library_file_id"),
                media_item_id = ?body.get("media_item_id"), "Playback quality metric identity is invalid or not visible"),
        }
        ok(json!({ "ok": true })).into_response()
    })
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

fn metric_media_id(
    store: &crate::Store,
    user_id: domain::UserId,
    body: &Value,
) -> Option<domain::MediaId> {
    let explicit = body["media_item_id"].as_str().and_then(resolve_media_id);
    if let Some(file) = body.get("library_file_id").filter(|file| !file.is_null()) {
        let file_id = file.as_str()?.parse::<domain::LedgerId>().ok()?;
        let file = store.visible_file(user_id, file_id).ok().flatten()?;
        return explicit
            .is_none_or(|id| id == file.media_id)
            .then_some(file.media_id);
    }
    let id = explicit?;
    let media = store.get_media(id).ok().flatten()?;
    let rows = store.file_coverage_for_media(id).ok()?;
    (!visible_coverages_for_media(store, &media, &rows, Some(user_id)).is_empty()).then_some(id)
}

/// GET/PUT /playback/policy — direct-play only; transcode flags persist in KV.
pub(crate) async fn get_policy(State(state): State<ApiState>) -> Response {
    // Review L2: policy lives in the KV table; read it off the executor.
    let result = store_read(&state, |store| -> Response {
        let prefs = store
            .get_setting("playback.policy")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| json!({}));
        ok(json!({
            "software_transcode_enabled": prefs["software_transcode_enabled"].as_bool().unwrap_or(false),
            "trickplay_enabled": prefs["trickplay_enabled"].as_bool().unwrap_or(false),
            "transcode_cache_enabled": prefs["transcode_cache_enabled"].as_bool().unwrap_or(false),
            "hardware_available": false,
            "hw_backends": [],
        }))
        .into_response()
    })
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

pub(crate) async fn put_policy(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    // Review L2: a KV write; keep the executor free while it waits for the lock.
    let result = store_read(&state, move |store| -> Response {
        let mut prefs = store
            .get_setting("playback.policy")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| json!({}));
        for key in [
            "software_transcode_enabled",
            "trickplay_enabled",
            "transcode_cache_enabled",
        ] {
            if let Some(value) = body.get(key) {
                prefs[key] = value.clone();
            }
        }
        if let Err(error) = store.put_setting("playback.policy", &prefs.to_string()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        ok(json!({
            "software_transcode_enabled": prefs["software_transcode_enabled"].as_bool().unwrap_or(false),
            "trickplay_enabled": prefs["trickplay_enabled"].as_bool().unwrap_or(false),
            "transcode_cache_enabled": prefs["transcode_cache_enabled"].as_bool().unwrap_or(false),
            "hardware_available": false,
            "hw_backends": [],
        }))
        .into_response()
    })
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

/// POST /playback/client-log — best-effort client telemetry sink.
pub(crate) async fn client_log(
    State(_state): State<ApiState>,
    Json(_body): Json<Value>,
) -> Response {
    ok(json!({ "ok": true })).into_response()
}

// ---------------------------------------------------------------------------
// UI prefs (unchanged)
// ---------------------------------------------------------------------------

fn ui_preferences_key(user_id: domain::UserId) -> String {
    format!("user.{user_id}.ui.preferences")
}

pub(crate) async fn get_ui_prefs(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    match store_read(&state, move |store| {
        let raw = store
            .get_setting(&ui_preferences_key(user_id))
            .ok()
            .flatten();
        let prefs = raw
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}));
        ok(prefs).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

pub(crate) async fn put_ui_prefs(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let result = store_read(&state, move |store| -> Response {
        let key = ui_preferences_key(user_id);
        let raw = store.get_setting(&key).ok().flatten();
        let mut prefs = raw
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}));
        if let (Some(obj), Some(new_obj)) = (prefs.as_object_mut(), body.as_object()) {
            for (k, v) in new_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
        if let Err(error) = store.put_setting(&key, &prefs.to_string()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        ok(prefs).into_response()
    })
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

pub(crate) fn session_timeout_secs() -> i64 {
    SESSION_TIMEOUT_SECS
}

/// GET /playback/hardware — 硬件转码能力（我们不转码：恒空）。
pub(crate) async fn hardware() -> Response {
    ok(json!({
        "hw_backend": null,
        "available": [],
    }))
    .into_response()
}

/// GET /playback/subtitles/{ledger_id}/{index} — 交付已探测的字幕文件
pub(crate) async fn get_subtitle_file(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::extract::Path((ledger_id, index)): axum::extract::Path<(String, u32)>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (row, media_title, file_path, file_id) = match store_read(&state, move |store| {
        let not_found = || err(StatusCode::NOT_FOUND, "subtitle.not_found", "字幕未找到");
        let Ok(file_id) = ledger_id.parse::<domain::LedgerId>() else {
            return Err(not_found());
        };
        // G07: 先校验该文件对当前用户可见，防止普通成员跨 Library 读取字幕。
        let Some(file) = store.visible_file(user_id, file_id).ok().flatten() else {
            return Err(not_found());
        };
        let Some(media) = store.get_media(file.media_id).ok().flatten() else {
            return Err(not_found());
        };
        let Ok(Some(tracks)) = store.get_file_meta(&file.file_id.to_string()) else {
            return Err(not_found());
        };
        let row = crate::subtitle_delivery::SubtitleRequest {
            ledger_id: file.file_id,
            source_path: file.path.clone(),
            kind: media.kind,
            tracks,
            index,
            wants_vtt: query
                .get("format")
                .map(|f| f.eq_ignore_ascii_case("vtt"))
                .unwrap_or(false),
        };
        let file_path = file.path.display().to_string();
        let fid = file.file_id;
        Ok((row, media.title, file_path, fid))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };

    match state.subtitle_delivery.deliver(&state, row).await {
        Ok(payload) => (
            [(axum::http::header::CONTENT_TYPE, payload.content_type)],
            payload.bytes,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(
                media = %media_title,
                path = %file_path,
                ledger_id = %file_id,
                index,
                %error,
                "交付字幕文件失败"
            );
            subtitle_error_response(error)
        }
    }
}

/// 字幕服务错误 → REST 状态码（原有 404/500 语义保留，新增 503/504/409）。
fn subtitle_error_response(error: crate::subtitle_delivery::SubtitleServiceError) -> Response {
    use crate::subtitle_delivery::SubtitleServiceError;
    use library::DeliveryError;
    match error {
        SubtitleServiceError::Delivery(DeliveryError::TrackNotFound) => {
            err(StatusCode::NOT_FOUND, "subtitle.not_found", "字幕轨不存在")
        }
        SubtitleServiceError::Delivery(DeliveryError::FileMissing | DeliveryError::Io(_)) => err(
            StatusCode::NOT_FOUND,
            "subtitle.file_missing",
            "字幕文件不存在",
        ),
        SubtitleServiceError::Delivery(DeliveryError::Extraction(msg)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subtitle.extraction_failed",
            &format!("提取内封字幕失败: {msg}"),
        ),
        SubtitleServiceError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "subtitle.busy",
            "字幕服务繁忙，请稍后重试",
        ),
        SubtitleServiceError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "subtitle.timeout",
            "字幕提取超时",
        ),
        SubtitleServiceError::Cancelled => {
            err(StatusCode::CONFLICT, "subtitle.cancelled", "字幕提取已取消")
        }
        SubtitleServiceError::Worker(message) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subtitle.worker",
            &format!("字幕提取任务异常: {message}"),
        ),
    }
}
