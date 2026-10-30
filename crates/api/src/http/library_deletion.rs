use crate::{
    http::{err, ok},
    management::ApiState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(crate) async fn status(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    super::store_wait::store_read(&state, move |store| {
        match store.library_removal_status(&id) {
            Ok(Some(operation)) => ok(serde_json::json!(operation)).into_response(),
            Ok(None) => err(
                StatusCode::NOT_FOUND,
                "library.removal_missing",
                "删除操作不存在",
            ),
            Err(error) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ),
        }
    })
    .await
    .unwrap_or_else(|error| error.response())
}
