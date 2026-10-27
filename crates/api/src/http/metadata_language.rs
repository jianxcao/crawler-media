//! Single authoritative default language; accepted changes expose durable rebuilds.
use crate::{
    http::{err, ok},
    management::ApiState,
};
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Change {
    expected_revision: i64,
    language: String,
}
pub(crate) async fn get(State(state): State<ApiState>) -> Response {
    match state.store.lock().metadata_language() {
        Ok(value) => ok(serde_json::json!(value)).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}
pub(crate) async fn patch(
    State(state): State<ApiState>,
    body: Result<Json<Change>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return err(
            StatusCode::BAD_REQUEST,
            "settings.invalid",
            "expected_revision and language are required; unknown fields are refused",
        );
    };
    let result = state.store.lock().change_metadata_language(
        body.expected_revision,
        &body.language,
        &uuid::Uuid::new_v4().to_string(),
        crate::job_loop::unix_now(),
    );
    match result {
        Ok(value) => {
            let changed = value.revision != body.expected_revision;
            if changed {
                for operation in &value.operations {
                    let library = operation.library_id.to_string();
                    state.library_work.block(&library, &operation.operation_id);
                    crate::storage_operations::spawn_driver(
                        &state,
                        &operation.operation_id,
                        &library,
                    );
                }
            }
            let status = if changed && !value.operations.is_empty() {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            (status, ok(serde_json::json!(value))).into_response()
        }
        Err(store::StoreError::Protected(message)) => {
            err(StatusCode::CONFLICT, "settings.conflict", &message)
        }
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}
