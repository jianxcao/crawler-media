use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use domain::MediaId;
use std::str::FromStr;

use crate::management::ApiState;
use crate::media_posters::PosterError;

pub(crate) async fn get_media_poster(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id_str): Path<String>,
) -> Response {
    let media_id = match MediaId::from_str(&id_str) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid media id").into_response(),
    };

    match state.media_posters.cached(media_id).await {
        Ok(Some(poster)) => {
            let mut response = super::library_artwork::image_response::conditional_image(
                &headers,
                poster.bytes.to_vec(),
                &poster.content_type,
                false,
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            );
            return response;
        }
        Ok(None) => {}
        Err(error) => return failure(error),
    }
    // Capture identity from the read pool before remote work. No writer lock or
    // reader lease travels into catalog lookup, image download, or cache writes.
    match super::store_wait::store_read(&state, move |store| store.get_media(media_id)).await {
        Ok(Ok(Some(media))) if media.tmdb_id.is_some() => {}
        Ok(Ok(_)) => return failure(PosterError::NotFound),
        Ok(Err(error)) => return failure(PosterError::Io(error.to_string())),
        Err(error) => return error.response(),
    }
    if let Err(error) = state.media_posters.clone().warm(media_id) {
        return failure(error);
    }
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, "poster cache warming").into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

fn failure(error: PosterError) -> Response {
    tracing::warn!(%error, "Media poster unavailable");
    let mut response = match error {
        PosterError::NotFound => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "poster not found",
        )
            .into_response(),
        PosterError::Timeout => {
            (StatusCode::GATEWAY_TIMEOUT, "poster fetch timed out").into_response()
        }
        PosterError::InvalidImage | PosterError::Upstream(_) => {
            (StatusCode::BAD_GATEWAY, "failed to fetch upstream poster").into_response()
        }
        PosterError::Io(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "poster io error").into_response()
        }
        PosterError::Busy => {
            (StatusCode::SERVICE_UNAVAILABLE, "poster service busy").into_response()
        }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
