use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;

use crate::auth::AuthUser;
use crate::dto::person_item_json;

use super::AppState;

pub(super) async fn person_by_name(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut person = state
        .provider
        .resolve_person(user.id, &name)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, person = %name, "failed to find Jellyfin person");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    if person.primary_image_url.is_none() {
        match state
            .provider
            .resolve_person_image_url(Some(user.id), &name)
            .await
        {
            Ok(image_url) => person.primary_image_url = image_url,
            Err(error) => {
                tracing::warn!(%error, person = %name, "failed to resolve Jellyfin person image metadata");
            }
        }
    }
    Ok(Json(person_item_json(&person)))
}
