//! Playback session selection (POST /playback/sessions) and its private helpers.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

use super::selection::resolve_playback_for;
use super::direct_decision;

/// POST /playback/sessions — direct decision with session_id null (tier 0).
pub(crate) async fn start_session(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Json(body): Json<Value>,
) -> Response {
    let (row, media, resolved, tracks) = {
        let store = state.store.lock();
        let Some((row, media, resolved)) = resolve_playback_for(&store, &body, user_id) else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let tracks = store.get_file_meta(&row.id.to_string()).ok().flatten();
        (row, media, resolved, tracks)
    };
    let decision = direct_decision(&row, &body, tracks.as_ref());
    if decision["outcome"].as_str() == Some("rejected") {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.unsupported",
            "设备不支持直接播放该视频编码，当前服务未配置实时转码",
        );
    }
    let start_ms = resolved.start_ms;
    let _ = crate::http::library::ensure_probe_enqueued(&state, &row);
    let device_id = body.get("device_id").and_then(|v| v.as_str()).unwrap_or("web-player");
    // 播放域时间一律走注入时钟。`issue_play_session` 的 `issued_at` 是**秒**
    // （Store 内部按 30 天秒数清理过期会话）。
    let issued_at = state.playback_clock.now_ms() / 1000;
    let play_session_id = {
        let store = state.store.lock();
        store
            .issue_play_session(user_id, device_id, row.id, issued_at)
            .ok()
    };
    tracing::info!(media_id = %media.id, file_id = %row.id, %user_id, start_ms, ?play_session_id, "Playback session selected");
    let stream_id = row.id.to_string().replace('-', "");
    let is_strm = std::path::Path::new(&row.path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));
    let stream_url = session_stream_url(&row, &stream_id, &token, is_strm);
    let (chapters, chapters_pending) = session_chapters(&state, &row, &media);
    let source = session_source(&row, is_strm);
    let subtitle_urls: Vec<String> = decision
        .get("subtitles")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .map(|sub| {
                    sub.get("delivery_url")
                        .and_then(|u| u.as_str())
                        .map(|url| format!("{url}?api_key={token}"))
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    let resume_status = match resolved.status {
        crate::store::ResumeStatus::Compatible => "compatible",
        crate::store::ResumeStatus::SourceChanged => "source_changed",
        crate::store::ResumeStatus::NoProgress => "no_progress",
        crate::store::ResumeStatus::AlreadyPlayed => "already_played",
    };
    let watch = json!({
        "position_ms": resolved.start_ms,
        "played": resolved.status == crate::store::ResumeStatus::AlreadyPlayed,
        "duration_ms": resolved.duration_ms,
        "audio_track": resolved.audio_track,
        "subtitle_track": resolved.subtitle_track,
    });
    ok(json!({
        "decision": decision,
        "session_id": null,
        "play_session_id": play_session_id,
        "stream_url": stream_url,
        "start_ms": start_ms,
        "resume_status": resume_status,
        "subtitle_urls": subtitle_urls,
        "hw_backend": null,
        "chapters": chapters,
        "chapters_pending": chapters_pending,
        "file_id": row.id.to_string(),
        "watch": watch,
        "source": source,
        "title": media.title,
    }))
    .into_response()
}

fn session_source(row: &domain::LedgerRow, is_strm: bool) -> Value {
    let size_bytes = (!is_strm)
        .then(|| std::fs::metadata(&row.path).ok().map(|m| m.len() as i64))
        .flatten();
    json!({
        "container": std::path::Path::new(&row.path).extension().and_then(|e| e.to_str()),
        "resolution": row.resolution,
        "video_codec": row.codec,
        "hdr": row.hdr,
        "bit_rate": null,
        "frame_rate": null,
        "size_bytes": size_bytes,
    })
}

fn session_stream_url(
    row: &domain::LedgerRow,
    stream_id: &str,
    token: &str,
    is_strm: bool,
) -> String {
    // STRM 优先给客户端直链；没有有效直链时回退本地流中转。
    if is_strm {
        if let Some(direct_url) = library::read_strm_url(std::path::Path::new(&row.path)) {
            return direct_url;
        }
    }
    format!("/Videos/{stream_id}/stream?api_key={token}")
}

fn session_chapters(
    state: &ApiState,
    row: &domain::LedgerRow,
    media: &domain::Media,
) -> (Vec<Value>, bool) {
    let cached_chapters = state
        .store
        .lock()
        .get_cached_chapters(&row.id.to_string())
        .ok()
        .flatten();
    let pending = cached_chapters.is_none();
    if pending {
        let state = state.clone();
        let row = row.clone();
        let media = media.clone();
        tokio::spawn(async move {
            let _ = crate::marker_resolver::resolve_item_chapters(&state, &row, &media, None).await;
        });
    }
    let chapters = cached_chapters
        .unwrap_or_default()
        .iter()
        .map(|chapter| json!({ "start_ms": chapter.start_ms, "title": chapter.title }))
        .collect();
    (chapters, pending)
}
