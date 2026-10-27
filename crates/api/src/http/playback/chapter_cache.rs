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
    file_id: String,
}

/// Lightweight cache read for the player after a cold session starts probing chapters.
pub(crate) async fn cached_chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::extract::Query(query): axum::extract::Query<ChapterCacheQuery>,
) -> Response {
    // Review L2: a cache read on the player's hot path; off the executor.
    let result = store_read(&state, move |store| -> Response {
        let file_id = match query.file_id.replace('-', "").parse::<domain::LedgerId>() {
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
        if !crate::http::library::file_visible_to_user(&store, &cov.file, &media, user_id) {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        }
        match store.get_cached_chapters(&cov.file.file_id.to_string()) {
            Ok(Some(chapters)) => ok(json!({
                "ready": true,
                "chapters": chapters.iter().map(|chapter| json!({
                    "start_ms": chapter.start_ms,
                    "title": chapter.title,
                })).collect::<Vec<_>>(),
            }))
            .into_response(),
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
