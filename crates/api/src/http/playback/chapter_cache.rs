//! Cached chapters for a player while background probing runs.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

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
    let store = state.store.lock();
    let Some(row) = store
        .get_ledger(&query.file_id.replace('-', ""))
        .ok()
        .flatten()
    else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    let Some(media) = store.get_media(row.media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    if !crate::http::library::row_visible_to_user(&store, &row, &media, user_id) {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    }
    match store.get_cached_chapters(&row.id.to_string()) {
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
}
