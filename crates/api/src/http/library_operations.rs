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
    match storage_operations::begin_library_integrity(&state, &library_id, false) {
        Ok(operation) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "ok": true, "data": operation })),
        )
            .into_response(),
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
    match storage_operations::begin_library_integrity(&state, &library_id, true) {
        Ok(operation) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "ok": true, "data": operation })),
        )
            .into_response(),
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

/// `GET /libraries/{id}/operations/{operation_id}`
pub(crate) async fn get_operation(
    State(state): State<ApiState>,
    Path((library_id, operation_id)): Path<(String, String)>,
) -> Response {
    match storage_operations::library_operation(&state, &library_id, &operation_id) {
        Ok(Some(operation)) => ok(operation).into_response(),
        Ok(None) => err(
            StatusCode::NOT_FOUND,
            "library.operation_not_found",
            "媒体库操作不存在",
        ),
        Err(error) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error),
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
    if !reads {
        let (mut parts, body) = request.into_parts();
        let library_id = library_path_param(&mut parts).await;
        let request = Request::from_parts(parts, body);
        if let Some(library_id) = library_id
            && let WriteVerdict::Reindexing { operation_id } =
                storage_operations::write_verdict(&state, &library_id)
        {
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
