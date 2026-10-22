//! Playback session selection (POST /playback/sessions) and its private helpers.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

use super::direct_decision;
use super::selection::resolve_playback_for;

/// POST /playback/sessions — direct decision with session_id null (tier 0).
pub(crate) async fn start_session(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(value) = body.get("start_ms") {
        if !value.as_i64().is_some_and(|start| start >= 0) {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "start_ms 必须是非负整数",
            );
        }
    }

    // Validate coordinates: movie cannot have explicit coordinates (even 0, 0 is invalid now);
    // TV S00E00 (season 0 episode 0) is valid.
    {
        let store = state.store.lock();
        if let Some(media_id) = body
            .get("media_item_id")
            .and_then(|v| v.as_str())
            .and_then(super::resolve_media_id)
        {
            if let Ok(Some(media)) = store.get_media(media_id) {
                if media.kind != domain::MediaKind::Tv {
                    let has_season = body
                        .get("season_number")
                        .or_else(|| body.get("season"))
                        .is_some();
                    let has_episode = body
                        .get("episode_number")
                        .or_else(|| body.get("episode"))
                        .is_some();
                    if has_season || has_episode {
                        return err(
                            StatusCode::BAD_REQUEST,
                            "playback.invalid",
                            "电影不能包含季集坐标",
                        );
                    }
                }
            }
        }
    }
    let (file, media, resolved, tracks) = {
        let store = state.store.lock();
        let Some((file, media, resolved)) = resolve_playback_for(&store, &body, user_id) else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let tracks = store
            .get_file_meta(&file.file_id.to_string())
            .ok()
            .flatten();
        (file, media, resolved, tracks)
    };
    let decision = direct_decision(&file, &body, tracks.as_ref());
    if decision["outcome"].as_str() == Some("rejected") {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.unsupported",
            "设备不支持直接播放该视频编码，当前服务未配置实时转码",
        );
    }
    let start_ms = resolved.start_ms;
    let coverage = domain::FileCoverage {
        file: file.clone(),
        units: vec![domain::UnitKey::new(
            resolved.target.unit.season,
            resolved.target.unit.episode,
        )],
    };
    let _ = crate::http::library::ensure_probe_enqueued(&state, &coverage);
    let device_id = body
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("web-player");
    // 播放域时间一律走注入时钟。`issue_play_session` 的 `issued_at` 是**秒**
    // （Store 内部按 30 天秒数清理过期会话）。
    let issued_at = state.playback_clock.now_ms() / 1000;
    let play_session_id = {
        let store = state.store.lock();
        store
            .issue_play_session(
                user_id,
                device_id,
                file.file_id,
                resolved.target.media_id,
                resolved.target.unit,
                &resolved.target.file.version,
                issued_at,
            )
            .ok()
    };
    tracing::info!(media_id = %media.id, file_id = %file.file_id, %user_id, start_ms, ?play_session_id, "Playback session selected");
    let is_strm = file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));
    let stream_url = session_stream_url(
        &file,
        play_session_id.as_deref(),
        device_id,
        &token,
        is_strm,
    );
    let (chapters, chapters_pending) = session_chapters(&state, &file, &media);
    let source = session_source(&file, is_strm);
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
        "file_id": file.file_id.to_string(),
        "watch": watch,
        "source": source,
        "title": media.title,
    }))
    .into_response()
}

fn session_source(file: &domain::LibraryFile, is_strm: bool) -> Value {
    let size_bytes = (!is_strm)
        .then(|| std::fs::metadata(&file.path).ok().map(|m| m.len() as i64))
        .flatten();
    json!({
        "container": file.path.extension().and_then(|e| e.to_str()),
        "resolution": file.resolution,
        "video_codec": file.codec,
        "hdr": file.hdr,
        "bit_rate": null,
        "frame_rate": null,
        "size_bytes": size_bytes,
    })
}

fn session_stream_url(
    file: &domain::LibraryFile,
    play_session_id: Option<&str>,
    device_id: &str,
    token: &str,
    is_strm: bool,
) -> String {
    // STRM 优先给客户端直链；没有有效直链时回退本地流中转。
    if is_strm {
        if let Some(direct_url) = library::read_strm_url(&file.path) {
            return direct_url;
        }
    }
    if let Some(psid) = play_session_id {
        format!("/api/v1/playback/sessions/{psid}/stream?device_id={device_id}&api_key={token}")
    } else {
        let stream_id = file.file_id.to_string().replace('-', "");
        format!("/Videos/{stream_id}/stream?api_key={token}")
    }
}

fn session_chapters(
    state: &ApiState,
    file: &domain::LibraryFile,
    media: &domain::Media,
) -> (Vec<Value>, bool) {
    let cached_chapters = state
        .store
        .lock()
        .get_cached_chapters(&file.file_id.to_string())
        .ok()
        .flatten();
    let pending = cached_chapters.is_none();
    if pending {
        let state = state.clone();
        let file = file.clone();
        let media = media.clone();
        tokio::spawn(async move {
            let coverage = domain::FileCoverage {
                file,
                units: Vec::new(),
            };
            let _ = crate::marker_resolver::resolve_item_chapters(&state, &coverage, &media, None)
                .await;
        });
    }
    let chapters = cached_chapters
        .unwrap_or_default()
        .iter()
        .map(|chapter| json!({ "start_ms": chapter.start_ms, "title": chapter.title }))
        .collect();
    (chapters, pending)
}
#[derive(serde::Deserialize)]
pub(crate) struct SessionStreamQuery {
    pub device_id: String,
}

pub(crate) async fn session_stream(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::extract::Path(play_session_id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<SessionStreamQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, StatusCode> {
    let binding = {
        let store = state.store.lock();
        match store.play_session_binding(user_id, &query.device_id, &play_session_id) {
            Ok(Some(b))
                if matches!(
                    b.state,
                    crate::store::PlaySessionIdState::Pending
                        | crate::store::PlaySessionIdState::Active
                ) =>
            {
                b
            }
            _ => return Err(StatusCode::NOT_FOUND),
        }
    };
    let Some(file_id_str) = binding.file_id.as_deref() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let (path, is_strm) = {
        let store = state.store.lock();
        let Ok(file_id) = file_id_str.parse::<domain::LedgerId>() else {
            return Err(StatusCode::NOT_FOUND);
        };
        let Some(file) = store.visible_file(user_id, file_id).ok().flatten() else {
            return Err(StatusCode::NOT_FOUND);
        };
        let library_id = file.library_id.to_string();
        if binding.media_id != file.media_id
            || binding.file_revision != Some(file.version.revision)
            || binding.source_version.as_deref() != Some(file.version.source_version.as_str())
            || binding.library_id.as_deref() != Some(library_id.as_str())
            || store.library_config_revision(&library_id).ok() != Some(binding.config_revision)
            || !store
                .get_file_units(file_id)
                .ok()
                .is_some_and(|units| units.contains(&binding.unit))
            || store
                .is_device_revoked(user_id, &query.device_id)
                .unwrap_or(true)
            || !store.library_accepts_work(&library_id).unwrap_or(false)
        {
            return Err(StatusCode::NOT_FOUND);
        }
        let is_strm = file
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
        (file.path, is_strm)
    };

    if is_strm {
        if let Some(direct_url) = library::read_strm_url(&path) {
            return Ok((
                StatusCode::FOUND,
                [(axum::http::header::LOCATION, direct_url)],
            )
                .into_response());
        } else {
            return Err(StatusCode::NOT_FOUND);
        }
    }

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let content_type = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        _ => "application/octet-stream",
    };

    let file_size = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();

    let range_header = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok());

    if let Some(header_value) = range_header {
        let Some(spec) = playback::parse_range(header_value, file_size) else {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (
                        axum::http::header::CONTENT_RANGE,
                        format!("bytes */{file_size}"),
                    ),
                    (axum::http::header::ACCEPT_RANGES, "bytes".into()),
                ],
                axum::body::Body::empty(),
            )
                .into_response());
        };

        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(tokio::io::SeekFrom::Start(spec.start))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let stream = tokio_util::io::ReaderStream::new(file.take(spec.len()));
        let content_range = format!("bytes {}-{}/{file_size}", spec.start, spec.end);
        let content_length = spec.len().to_string();

        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (axum::http::header::CONTENT_TYPE, content_type.to_string()),
                (axum::http::header::CONTENT_RANGE, content_range),
                (axum::http::header::CONTENT_LENGTH, content_length),
                (axum::http::header::ACCEPT_RANGES, "bytes".to_string()),
            ],
            axum::body::Body::from_stream(stream),
        )
            .into_response());
    }

    let stream = tokio_util::io::ReaderStream::new(file);
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, content_type.to_string()),
            (axum::http::header::CONTENT_LENGTH, file_size.to_string()),
            (axum::http::header::ACCEPT_RANGES, "bytes".to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}
