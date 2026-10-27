//! Cached chapters for a player while background probing runs.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::http::store_wait::store_read;
use crate::http::{err, ok};
use crate::management::ApiState;

#[derive(serde::Deserialize)]
pub(crate) struct ChapterCacheQuery {
    play_session_id: String,
    device_id: String,
}

/// Lightweight cache read for the player after a cold session starts probing chapters.
pub(crate) async fn cached_chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::extract::Query(query): axum::extract::Query<ChapterCacheQuery>,
) -> Response {
    // Review L2: a cache read on the player's hot path; off the executor.
    let result = store_read(&state, move |store| -> Response {
        let Some(user_id) = user_id else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let binding =
            match store.play_session_binding(user_id, &query.device_id, &query.play_session_id) {
                Ok(Some(binding))
                    if matches!(
                        binding.state,
                        store::PlaySessionIdState::Pending | store::PlaySessionIdState::Active
                    ) =>
                {
                    binding
                }
                _ => return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在"),
            };
        let file_id = match binding
            .file_id
            .as_deref()
            .unwrap_or_default()
            .parse::<domain::LedgerId>()
        {
            Ok(id) => id,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid",
                    "无效的 file_id",
                );
            }
        };
        let Some(cov) = store.file_coverage_by_id(file_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let Some(media) = store.get_media(cov.file.media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let library_id = cov.file.library_id.to_string();
        if !crate::http::library::file_visible_to_user(&store, &cov.file, &media, Some(user_id))
            || binding.media_id != cov.file.media_id
            || binding.file_revision != Some(cov.file.version.revision)
            || binding.source_version.as_deref() != Some(cov.file.version.source_version.as_str())
            || binding.library_id.as_deref() != Some(library_id.as_str())
            || store.library_config_revision(&library_id).ok() != Some(binding.config_revision)
            || !cov.units.contains(&binding.unit)
            || store
                .is_device_revoked(user_id, &query.device_id)
                .unwrap_or(true)
            || !store.library_accepts_work(&library_id).unwrap_or(false)
        {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        }
        match store.get_cached_chapters(&cov.file.file_id.to_string()) {
            Ok(Some(mut chapters)) => {
                let marker = crate::marker_resolver::unit_marker(
                    store,
                    &media,
                    binding.unit,
                    cov.units.len(),
                );
                crate::marker_resolver::append_marker_chapters(
                    &mut chapters,
                    marker.as_ref(),
                    cov.file.duration_ms,
                );
                ok(json!({
                    "ready": true,
                    "chapters": chapters.iter().map(|chapter| json!({
                        "start_ms": chapter.start_ms,
                        "title": chapter.title,
                    })).collect::<Vec<_>>(),
                }))
                .into_response()
            }
            Ok(None) => ok(json!({ "ready": false, "chapters": [] })).into_response(),
            Err(error) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ),
        }
    })
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}
