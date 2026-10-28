use crate::{
    ApiState,
    http::{err, ok},
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Bind an unidentified file to an existing Media in place (no bytes move);
/// only a path strictly inside a Library root is accepted.
pub(crate) fn bind_existing(state: &ApiState, id: &str, path: &str) -> Response {
    let path = path.trim();
    if path.is_empty() {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "path 必填");
    }
    let media_id = match id.parse::<domain::MediaId>() {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "media id 无效"),
    };
    let (media, source) = match super::admitted_store(state, |store| {
        let media = store
            .get_media(media_id)
            .map_err(super::store_response)?
            .ok_or_else(|| {
                err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "影视条目不存在",
                )
            })?;
        let source = store
            .get_unidentified(path)
            .map_err(super::store_response)?
            .ok_or_else(|| {
                err(
                    StatusCode::NOT_FOUND,
                    "unidentified.missing",
                    "Unidentified not found",
                )
            })?;
        Ok((media, source))
    }) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let facts = crate::claim::ClaimedFacts::for_path(media.kind, std::path::Path::new(path));
    let file = match super::publication::publish_http_unidentified(
        state,
        &media,
        std::path::Path::new(path),
        &facts,
        &source,
        None,
    ) {
        Ok(file) => file,
        Err(response) => return response,
    };
    crate::http::library::enqueue_probes_for_files(state, std::slice::from_ref(&file));
    ok(json!({
        "media_item_id": media.id.to_string(),
        "path": path,
    }))
    .into_response()
}
