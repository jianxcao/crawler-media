use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use std::sync::Arc;

use crate::auth::authenticate_optional;
use crate::provider::MediaServerProvider;

use super::super::AppState;

// 从 images.rs 注册兄弟文件模块，避免改动本任务写范围之外的 `lib.rs`。
#[path = "../../image_cache.rs"]
pub(crate) mod image_cache;

use image_cache::conditional_image;

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
        .route("/Genres/{name}/Images/Primary", get(genre_primary_image))
        .route("/Genres/{name}/Images/Backdrop", get(genre_primary_image))
        .route("/Studios/{name}/Images/Primary", get(studio_primary_image))
        .route("/Studios/{name}/Images/Backdrop", get(studio_primary_image))
        .route_layer(axum::middleware::from_fn(move |req, next| {
            let provider = image_provider.clone();
            authenticate_optional(provider, req, next)
        }))
        .with_state(AppState::new(provider))
}

async fn person_primary_image(
    State(state): State<AppState>,
    _headers: HeaderMap,
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
async fn genre_primary_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    user: Option<Extension<Option<domain::UserId>>>,
    Path(name): Path<String>,
) -> Result<Response, StatusCode> {
    entity_image(
        state,
        headers,
        user.and_then(|extension| extension.0),
        crate::provider::EntityKind::Genre,
        &name,
    )
    .await
}
async fn studio_primary_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    user: Option<Extension<Option<domain::UserId>>>,
    Path(name): Path<String>,
) -> Result<Response, StatusCode> {
    entity_image(
        state,
        headers,
        user.and_then(|extension| extension.0),
        crate::provider::EntityKind::Studio,
        &name,
    )
    .await
}
async fn entity_image(
    state: AppState,
    headers: HeaderMap,
    user: Option<domain::UserId>,
    kind: crate::provider::EntityKind,
    name: &str,
) -> Result<Response, StatusCode> {
    let bytes = state
        .provider
        .resolve_entity_image(user, kind, name)
        .await
        .map_err(|error| {
            tracing::warn!(%error,entity=%name,"failed to deliver Jellyfin entity artwork");
            StatusCode::NOT_FOUND
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(image_response(&headers, bytes))
}

async fn primary_image(
    State(state): State<AppState>,
    headers: HeaderMap,
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
        return Ok(image_response(&headers, bytes));
    }
    match state.provider.resolve_poster_bytes(user_id, &id).await {
        Ok(Some(bytes)) => return Ok(image_response(&headers, bytes)),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, item_id = %id, "failed to read Jellyfin primary image");
        }
    }
    if let Ok(res) = genre_primary_image(
        State(state.clone()),
        headers.clone(),
        user_id.map(|u| Extension(Some(u))),
        Path(id.clone()),
    )
    .await
    {
        return Ok(res);
    }
    if let Ok(res) = studio_primary_image(
        State(state.clone()),
        headers.clone(),
        user_id.map(|u| Extension(Some(u))),
        Path(id.clone()),
    )
    .await
    {
        return Ok(res);
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
    headers: HeaderMap,
    user_id: Option<Extension<Option<domain::UserId>>>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let user_id = user_id.and_then(|extension| extension.0);
    resolve_backdrop(state, headers, user_id, id).await
}

async fn backdrop_image_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    user_id: Option<Extension<Option<domain::UserId>>>,
    Path((id, _index)): Path<(String, u32)>,
) -> Result<Response, StatusCode> {
    let user_id = user_id.and_then(|extension| extension.0);
    resolve_backdrop(state, headers, user_id, id).await
}

async fn resolve_backdrop(
    state: AppState,
    headers: HeaderMap,
    user_id: Option<domain::UserId>,
    id: String,
) -> Result<Response, StatusCode> {
    // 先取目标条目自身的 backdrop：自身通常就有图，避免为了拿图枚举整库。
    match state.provider.resolve_backdrop_bytes(user_id, &id).await {
        Ok(Some(bytes)) => return Ok(image_response(&headers, bytes)),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, item_id = %id, "failed to read Jellyfin backdrop image")
        }
    }
    // 只有自身没有图时才回落到 genre/studio 定向投影读。
    if let Ok(res) = genre_primary_image(
        State(state.clone()),
        headers.clone(),
        user_id.map(|u| Extension(Some(u))),
        Path(id.clone()),
    )
    .await
    {
        return Ok(res);
    }
    Err(StatusCode::NOT_FOUND)
}

/// 授权/可见性判定已在调用方完成；这里只做 validator 比较与响应组装。
fn image_response(headers: &HeaderMap, bytes: Vec<u8>) -> Response {
    let content_type = image_content_type(&bytes);
    conditional_image(headers, bytes, content_type, true)
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
