//! Playback session selection (POST /playback/sessions) and its private helpers.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::store_wait::{store_read, store_write};
use crate::http::{err, ok};
use crate::management::ApiState;

use super::direct_decision;
use super::request::parse_selection;
use super::selection::resolve_playback_for;

/// POST /playback/sessions — direct decision with session_id null (tier 0).
pub(crate) async fn start_session(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Json(body): Json<Value>,
) -> Response {
    let selection = match parse_selection(&body) {
        Ok(selection) => selection,
        Err(error) => return error.response(),
    };
    // Review L2: session selection resolves the ledger row; a contended lock must
    // not park the worker that serves every other request on this runtime.
    let (file, media, resolved, tracks) = match store_read(&state, move |store| {
        let (file, media, resolved) = match resolve_playback_for(store, &selection, user_id) {
            Ok(resolved) => resolved,
            Err(error) => return Err(error.response()),
        };
        let tracks = store
            .get_file_meta(&file.file_id.to_string())
            .ok()
            .flatten();
        Ok((file, media, resolved, tracks))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
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
    let file_id = file.file_id;
    let coverage = store_read(&state, move |store| {
        store.file_coverage_by_id(file_id).ok().flatten()
    })
    .await
    .ok()
    .flatten();
    let device_id = body
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("web-player");
    let play_session_id = match issue_binding(&state, user_id, device_id, resolved.clone()).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(coverage) = coverage {
        let _ = crate::http::library::ensure_probe_enqueued(&state, &coverage).await;
    }
    tracing::info!(media_id = %media.id, file_id = %file.file_id, %user_id, start_ms, ?play_session_id, "Playback session selected");
    let is_strm = file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));
    let stream_url = session_stream_url(&file, &play_session_id, device_id, &token, is_strm);
    let (chapters, chapters_pending) =
        session_chapters(&state, &file, resolved.target.unit, &media).await;
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
    let resume_status = resume_status_name(resolved.status);
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

async fn issue_binding(
    state: &ApiState,
    user: domain::UserId,
    device: &str,
    resolved: crate::store::ResolvedResume,
) -> Result<String, Response> {
    let file = resolved.target.file.clone();
    let media_id = resolved.target.media_id;
    let unit = resolved.target.unit;
    let version = file.version.clone();
    let now = state.playback_clock.now_ms() / 1000;
    let device = device.to_string();
    let issued = store_write(state, move |store| {
            store
                .issue_play_session(
                    user,
                    &device,
                    file.file_id,
                    media_id,
                    unit,
                    &version,
                    now,
                )
                .map_err(|error| {
                    tracing::error!(%error,%user,device,file_id=%file.file_id,"Playback session issuance failed");
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        "无法签发播放会话",
                    )
                })
    })
    .await;
    issued?
}

fn session_stream_url(
    file: &domain::LibraryFile,
    play_session_id: &str,
    device_id: &str,
    token: &str,
    is_strm: bool,
) -> String {
    // STRM 优先给客户端直链；没有有效直链时回退本地流中转。
    if is_strm && let Some(direct_url) = library::read_strm_url(&file.path) {
        return direct_url;
    }
    format!(
        "/api/v1/playback/sessions/{play_session_id}/stream?device_id={device_id}&api_key={token}"
    )
}

async fn session_chapters(
    state: &ApiState,
    file: &domain::LibraryFile,
    unit: domain::UnitKey,
    media: &domain::Media,
) -> (Vec<Value>, bool) {
    // Chapters decorate the session grant; a contended Store must not fail it.
    let file_id = file.file_id;
    let expected = file.version.clone();
    let cached_media = media.clone();
    let cached_chapters = match store_read(state, move |store| {
        let coverage = store.file_coverage_by_id(file_id).ok().flatten()?;
        if coverage.file.version != expected || !coverage.units.contains(&unit) {
            return None;
        }
        let mut cached = store
            .get_cached_chapters(&file_id.to_string())
            .ok()
            .flatten();
        if let Some(ref mut chapters) = cached {
            let marker = crate::marker_resolver::unit_marker(
                store,
                &cached_media,
                unit,
                coverage.units.len(),
            );
            crate::marker_resolver::append_marker_chapters(
                chapters,
                marker.as_ref(),
                coverage.file.duration_ms,
            );
        }
        cached
    })
    .await
    {
        Ok(cached) => cached,
        Err(error) => {
            tracing::warn!(
                ?error,
                "读取章节缓存失败，本次按待生成返回；章节仍在后台生成"
            );
            None
        }
    };
    let pending = cached_chapters.is_none();
    if pending {
        let state = state.clone();
        let file = file.clone();
        let media = media.clone();
        tokio::spawn(async move {
            let file_id = file.file_id;
            let coverage = store_read(&state, move |store| {
                store.file_coverage_by_id(file_id).ok().flatten()
            })
            .await;
            if let Ok(Some(coverage)) = coverage {
                let _ = crate::marker_resolver::resolve_item_chapters(
                    &state, &coverage, unit, &media, None,
                )
                .await;
            }
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

fn session_source_path(
    store: &crate::Store,
    user_id: domain::UserId,
    device_id: &str,
    play_session_id: &str,
) -> Result<(std::path::PathBuf, bool), StatusCode> {
    let binding = match store.play_session_binding(user_id, device_id, play_session_id) {
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
    };
    let Some(file_id_str) = binding.file_id.as_deref() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let (path, is_strm) = {
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
            || store.is_device_revoked(user_id, device_id).unwrap_or(true)
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

    Ok((path, is_strm))
}

pub(crate) async fn session_stream(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::extract::Path(play_session_id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<SessionStreamQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, StatusCode> {
    // Review L2: binding validation is a Store read on the streaming hot path.
    let device_id = query.device_id.clone();
    let (path, is_strm) = match store_read(&state, move |store| {
        session_source_path(store, user_id, &device_id, &play_session_id)
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(status)) => return Err(status),
        Err(error) => {
            tracing::warn!(?error, "播放流绑定校验未在预算内完成，请求按繁忙重试");
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
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

pub(super) fn resume_status_name(status: crate::store::ResumeStatus) -> &'static str {
    match status {
        crate::store::ResumeStatus::Compatible => "compatible",
        crate::store::ResumeStatus::SourceChanged => "source_changed",
        crate::store::ResumeStatus::NoProgress => "no_progress",
        crate::store::ResumeStatus::AlreadyPlayed => "already_played",
    }
}
