use axum::Router;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use std::sync::Arc;

use crate::auth::authenticate_optional;
use crate::provider::MediaServerProvider;

use super::super::AppState;

pub(super) fn routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    let image_provider = provider.clone();
    Router::new()
        .route("/Items/{id}/Images/Primary", get(primary_image))
        .route("/Items/{id}/Images/Thumb", get(primary_image))
        .route("/Items/{id}/Images/Backdrop", get(backdrop_image))
        .route(
            "/Items/{id}/Images/Backdrop/{index}",
            get(backdrop_image_index),
        )
        .route("/Persons/{name}/Images/Primary", get(person_primary_image))
        .route_layer(axum::middleware::from_fn(move |req, next| {
            let provider = image_provider.clone();
            authenticate_optional(provider, req, next)
        }))
        .with_state(AppState::new(provider))
}

async fn person_primary_image(
    State(state): State<AppState>,
    user_id: Option<Extension<Option<domain::UserId>>>,
    Path(name): Path<String>,
) -> Result<Response, StatusCode> {
    let user_id = user_id.and_then(|extension| extension.0);
    match state
        .provider
        .resolve_person_image_url(user_id, &name)
        .await
    {
        Ok(Some(url)) => Ok(Redirect::temporary(&url).into_response()),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, person = %name, "failed to resolve Jellyfin person image");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

async fn primary_image(
    State(state): State<AppState>,
    user_id: Option<Extension<Option<domain::UserId>>>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let user_id = user_id.and_then(|extension| extension.0);
    match state.provider.resolve_primary_image_url(user_id, &id).await {
        Ok(Some(url)) => return Ok(Redirect::temporary(&url).into_response()),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, item_id = %id, "failed to resolve Jellyfin still image URL")
        }
    }
    if let Ok(Some(bytes)) = state.provider.resolve_cover_bytes(user_id, &id).await {
        return Ok(image_response(bytes));
    }
    match state.provider.resolve_poster_bytes(user_id, &id).await {
        Ok(Some(bytes)) => return Ok(image_response(bytes)),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, item_id = %id, "failed to read Jellyfin primary image");
        }
    }
    match state.provider.resolve_person_image_url(user_id, &id).await {
        Ok(Some(url)) => Ok(Redirect::temporary(&url).into_response()),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, person = %id, "failed to resolve Jellyfin person image by item id");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

async fn backdrop_image(
    State(state): State<AppState>,
    user_id: Option<Extension<Option<domain::UserId>>>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let user_id = user_id.and_then(|extension| extension.0);
    resolve_backdrop(state, user_id, id).await
}

async fn backdrop_image_index(
    State(state): State<AppState>,
    user_id: Option<Extension<Option<domain::UserId>>>,
    Path((id, _index)): Path<(String, u32)>,
) -> Result<Response, StatusCode> {
    let user_id = user_id.and_then(|extension| extension.0);
    resolve_backdrop(state, user_id, id).await
}

async fn resolve_backdrop(
    state: AppState,
    user_id: Option<domain::UserId>,
    id: String,
) -> Result<Response, StatusCode> {
    match state.provider.resolve_backdrop_bytes(user_id, &id).await {
        Ok(Some(bytes)) => Ok(image_response(bytes)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, item_id = %id, "failed to read Jellyfin backdrop image");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

fn image_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, image_content_type(&bytes))],
        Body::from(bytes),
    )
        .into_response()
}

fn image_content_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}
