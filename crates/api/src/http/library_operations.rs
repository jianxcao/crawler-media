//! HTTP surface of persistent Library reconfiguration: poll one operation, and refuse
//! writes that would race the running one.
//!
//! `PATCH /libraries/{id}` answers `202` with the same body shape this module's `GET`
//! returns, so a client stores the id once and then polls. The write gate lives here
//! (not in `library_config`) because it must run *before* any handler mutates a field:
//! a refused rename must leave the Library untouched.

use crate::{
    http::{err, ok},
    management::ApiState,
    storage_operations::{self, WriteVerdict},
};
use axum::{
    Json,
    extract::{FromRequestParts, Path, RawPathParams, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// `POST /libraries/{id}/integrity-checks`
pub(crate) async fn integrity_checks(
    State(state): State<ApiState>,
    Path(library_id): Path<String>,
) -> Response {
    begin_integrity(state, library_id, false).await
}

async fn begin_integrity(state: ApiState, library_id: String, repair: bool) -> Response {
    let worker = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || {
            storage_operations::begin_library_integrity(&worker, &library_id, repair)
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => return super::library::browse_work_error(error),
    };
    match result {
        Ok(operation) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "ok": true, "data": operation })),
        )
            .into_response(),
        Err(storage_operations::StorageOperationError::Busy) => super::store_wait::busy_response(),
        Err(storage_operations::StorageOperationError::Admission(response)) => response,
        Err(storage_operations::StorageOperationError::MissingLibrary) => {
            err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在")
        }
        Err(storage_operations::StorageOperationError::Reindexing { operation_id }) => {
            reindexing_conflict(operation_id.as_deref())
        }
        Err(storage_operations::StorageOperationError::Invalid(message)) => {
            err(StatusCode::BAD_REQUEST, "library.invalid", &message)
        }
        Err(storage_operations::StorageOperationError::Store(error)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// `POST /libraries/{id}/integrity-repairs`
pub(crate) async fn integrity_repairs(
    State(state): State<ApiState>,
    Path(library_id): Path<String>,
) -> Response {
    begin_integrity(state, library_id, true).await
}

/// `GET /libraries/{id}/operations/{operation_id}`
pub(crate) async fn get_operation(
    State(state): State<ApiState>,
    Path((library_id, operation_id)): Path<(String, String)>,
) -> Response {
    let read = super::store_wait::store_read(&state, move |store| {
        store.library_operation_view(&operation_id).map(|view| {
            view.filter(|view| view.library_id == library_id)
                .map(|view| storage_operations::operation_json(&view))
        })
    })
    .await;
    let result = match read {
        Ok(result) => result,
        Err(error) => return error.response(),
    };
    match result {
        Ok(Some(operation)) => ok(operation).into_response(),
        Ok(None) => err(
            StatusCode::NOT_FOUND,
            "library.operation_not_found",
            "媒体库操作不存在",
        ),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// Refuse writes to a Library whose reconfiguration is still running.
///
/// Reads are never blocked: observing the operation, listing files, or previewing a
/// migration all stay available, which is what makes polling possible. Routes without an
/// `{id}` capture (library creation, reordering, routing options) pass straight through.
pub(crate) async fn reject_conflicting_writes(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    let reads = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    // Only this exact matched retry route delegates validation to its handler.
    let retry = request.method() == Method::POST
        && request
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .is_some_and(|path| {
                path.as_str() == "/api/v1/libraries/{id}/operations/{operation_id}/retry"
                    || path.as_str() == "/libraries/{id}/operations/{operation_id}/retry"
            });
    if !reads && !retry {
        let (mut parts, body) = request.into_parts();
        let library_id = library_path_param(&mut parts).await;
        let request = Request::from_parts(parts, body);
        let verdict = if let Some(library) = library_id.clone() {
            match super::store_wait::store_read(&state, move |store| {
                storage_operations::write_verdict_for_store(store, &library)
            })
            .await
            {
                Ok(verdict) => verdict,
                Err(error) => return error.response(),
            }
        } else {
            WriteVerdict::Proceed
        };
        if let WriteVerdict::Reindexing { operation_id } = verdict {
            let library_id = library_id.unwrap_or_default();
            tracing::info!(
                %library_id,
                operation_id = ?operation_id,
                method = %request.method(),
                path = %request.uri().path(),
                "媒体库重建索引中，拒绝并发写请求"
            );
            return reindexing_conflict(operation_id.as_deref());
        }
        return next.run(request).await;
    }
    next.run(request).await
}

/// The `{id}` capture of the matched route, if this route has one.
///
/// `RawPathParams` is the only public way to read captures in middleware: the router
/// stores them as a private extension, so both this extraction and a missing-params
/// rejection are mapped to `None` (a route without `{id}` simply has no Library to
/// check).
async fn library_path_param(parts: &mut axum::http::request::Parts) -> Option<String> {
    let params = RawPathParams::from_request_parts(parts, &()).await.ok()?;
    params
        .iter()
        .find(|(key, _)| *key == "id")
        .map(|(_, value)| value.to_string())
}

/// `409 library.reindexing` with the extra `operation_id` key clients poll with.
pub(crate) fn reindexing_conflict(operation_id: Option<&str>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "ok": false,
            "error": {
                "code": "library.reindexing",
                "message": "媒体库正在重建索引，请稍后重试",
                "operation_id": operation_id,
            },
        })),
    )
        .into_response()
}

/// Retry only the persisted failed owner; never register another operation.
pub(crate) async fn retry_operation(
    State(state): State<ApiState>,
    Path((library, operation)): Path<(String, String)>,
) -> Response {
    let worker = state.clone();
    match state
        .http_work
        .run_to_completion(move || retry_blocking(worker, library, operation))
        .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn retry_blocking(state: ApiState, library: String, operation: String) -> Response {
    let view = super::store_wait::with_store_read(&state, |store| {
        match store.library_operation_view(&operation) {
            Ok(Some(view)) if view.library_id == library => {
                if view.state != "failed"
                    || !matches!(
                        view.kind.as_str(),
                        "roots" | "locale" | "default_locale" | "integrity"
                    )
                {
                    return Err(err(
                        StatusCode::CONFLICT,
                        "library.operation_not_retryable",
                        "operation is not a failed rebuild",
                    ));
                }
                if store
                    .active_library_operation(&library)
                    .ok()
                    .flatten()
                    .as_deref()
                    != Some(operation.as_str())
                {
                    return Err(err(
                        StatusCode::CONFLICT,
                        "library.operation_not_owner",
                        "operation does not own this Library",
                    ));
                }
                Ok(view)
            }
            Ok(_) => Err(err(
                StatusCode::NOT_FOUND,
                "library.operation_not_found",
                "operation not found",
            )),
            Err(error) => Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            )),
        }
    });
    let view = match view {
        Ok(Ok(view)) => view,
        Ok(Err(response)) | Err(response) => return response,
    };
    if let Err(response) = state
        .require_writable()
        .and_then(|()| state.require_write_admitted())
    {
        return response;
    }
    state.library_work.block(&library, &operation);
    storage_operations::spawn_driver(&state, &operation, &library);
    (
        StatusCode::ACCEPTED,
        ok(serde_json::json!({"operation_id":view.operation_id,"state":view.state})),
    )
        .into_response()
}
