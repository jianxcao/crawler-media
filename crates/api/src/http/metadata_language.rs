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
    let result =
        match super::store_wait::store_read(&state, |store| store.metadata_language()).await {
            Ok(result) => result,
            Err(error) => return error.response(),
        };
    match result {
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
    let work_state = state.clone();
    let expected_revision = body.expected_revision;
    let result = state
        .http_work
        .run_to_completion(move || {
            let result = super::store_wait::with_bounded_store(work_state.store(), |store| {
                store.change_metadata_language(
                    expected_revision,
                    &body.language,
                    &uuid::Uuid::new_v4().to_string(),
                    work_state.playback_clock.now_ms(),
                )
            })
            .ok_or(())?;
            if let Ok(value) = &result {
                for operation in &value.operations {
                    let library = operation.library_id.to_string();
                    work_state
                        .library_work
                        .block(&library, &operation.operation_id);
                    crate::storage_operations::spawn_driver(
                        &work_state,
                        &operation.operation_id,
                        &library,
                    );
                }
            }
            Ok::<_, ()>(result)
        })
        .await;
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(())) => return super::store_wait::busy_response(),
        Err(error) => return super::library::browse_work_error(error),
    };
    match result {
        Ok(value) => {
            let changed = value.revision != expected_revision;
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
