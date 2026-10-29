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

    match state.media_posters.get(media_id).await {
        Ok(poster) => {
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
            response
        }
        Err(error) => failure(error),
    }
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
