use super::*;
use axum::body::Bytes;
use store::{PreparedLibraryRoots, StoreError, library_operations::RootOperationRetryError};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryRoots {
    expected_revision: i64,
    root_paths: Vec<String>,
}

type RootCorrection = Option<(i64, PreparedLibraryRoots)>;

async fn prepare_correction(state: &ApiState, body: &[u8]) -> Result<RootCorrection, Response> {
    if body.is_empty() {
        return Ok(None);
    }
    let request: RetryRoots =
        serde_json::from_slice(body).map_err(|error| invalid(&error.to_string()))?;
    if request.root_paths.is_empty() || request.root_paths.iter().any(|root| root.trim().is_empty())
    {
        return Err(invalid(
            "root_paths must contain at least one nonempty path",
        ));
    }
    match state
        .fs_work
        .run_to_completion(move || {
            let refs = request
                .root_paths
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            PreparedLibraryRoots::observe(&refs)
                .map(|roots| Some((request.expected_revision, roots)))
        })
        .await
    {
        Ok(Ok(roots)) => Ok(roots),
        Ok(Err(error)) => Err(invalid(&error.to_string())),
        Err(error) => Err(crate::http::library::browse_work_error(error)),
    }
}

fn invalid(message: &str) -> Response {
    err(
        StatusCode::BAD_REQUEST,
        "library.invalid_operation",
        message,
    )
}

/// Retry only the persisted failed owner; never register another operation.
pub(crate) async fn retry_operation(
    State(state): State<ApiState>,
    Path((library, operation)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let correction = match prepare_correction(&state, &body).await {
        Ok(correction) => correction,
        Err(response) => return response,
    };
    let worker = state.clone();
    match state
        .http_work
        .run_to_completion(move || retry_blocking(worker, library, operation, correction))
        .await
    {
        Ok(response) => response,
        Err(error) => crate::http::library::browse_work_error(error),
    }
}

fn retry_blocking(
    state: ApiState,
    library: String,
    operation: String,
    correction: RootCorrection,
) -> Response {
    let view = match retryable_operation(&state, &library, &operation) {
        Ok(view) => view,
        Err(response) => return response,
    };
    if let Err(response) = state
        .require_writable()
        .and_then(|()| state.require_write_admitted())
    {
        return response;
    }
    if !state.library_work.claim_driver(&operation) {
        return err(
            StatusCode::CONFLICT,
            "library.operation_running",
            "operation driver is already running",
        );
    }
    let admitted = validate_claimed_owner(&state, &library, &operation)
        .and_then(|()| correct_plan(&state, &operation, correction));
    if let Err(response) = admitted {
        state.library_work.release_driver(&operation);
        return response;
    }
    state.library_work.block(&library, &operation);
    storage_operations::spawn_claimed_driver(&state, &operation, &library);
    (
        StatusCode::ACCEPTED,
        ok(serde_json::json!({"operation_id":view.operation_id,"state":view.state})),
    )
        .into_response()
}

fn retryable_operation(
    state: &ApiState,
    library: &str,
    operation: &str,
) -> Result<store::library_operations::LibraryOperationView, Response> {
    crate::http::store_wait::with_store_read(state, &[store::Db::Library], |store| {
        let view = store
            .library_operation_view(operation)
            .map_err(store_error)?
            .filter(|view| view.library_id == library)
            .ok_or_else(|| {
                err(
                    StatusCode::NOT_FOUND,
                    "library.operation_not_found",
                    "operation not found",
                )
            })?;
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
            .active_library_operation(library)
            .map_err(store_error)?
            .as_deref()
            != Some(operation)
        {
            return Err(not_owner());
        }
        Ok(view)
    })?
}

fn validate_claimed_owner(
    state: &ApiState,
    library: &str,
    operation: &str,
) -> Result<(), Response> {
    crate::http::store_wait::with_store_read(state, &[store::Db::Library], |store| {
        let current = store
            .library_operation_view(operation)
            .map_err(store_error)?;
        let owner = store
            .active_library_operation(library)
            .map_err(store_error)?;
        if current.is_some_and(|current| {
            current.library_id == library
                && current.state == "failed"
                && owner.as_deref() == Some(operation)
        }) {
            Ok(())
        } else {
            Err(not_owner())
        }
    })?
}

fn correct_plan(
    state: &ApiState,
    operation: &str,
    correction: RootCorrection,
) -> Result<(), Response> {
    let Some((revision, roots)) = correction else {
        return Ok(());
    };
    crate::http::store_wait::with_foreground_store(state, |store| {
        store.replace_failed_roots_operation(
            operation,
            revision,
            &roots,
            state.playback_clock.now_ms(),
        )
    })?
    .map(|_| ())
    .map_err(correction_error)
}

fn not_owner() -> Response {
    err(
        StatusCode::CONFLICT,
        "library.operation_not_owner",
        "operation is no longer the failed Library owner",
    )
}

fn store_error(error: StoreError) -> Response {
    tracing::error!(%error, "failed Library operation retry read");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

fn correction_error(error: RootOperationRetryError) -> Response {
    match error {
        RootOperationRetryError::NotOwner => err(
            StatusCode::CONFLICT,
            "library.operation_not_owner",
            &error.to_string(),
        ),
        RootOperationRetryError::NotReplannable => err(
            StatusCode::CONFLICT,
            "library.operation_not_replannable",
            &error.to_string(),
        ),
        RootOperationRetryError::Store(StoreError::Conflict(message)) => {
            err(StatusCode::CONFLICT, "library.root_conflict", &message)
        }
        RootOperationRetryError::Store(StoreError::Invalid(message)) => invalid(&message),
        RootOperationRetryError::Store(StoreError::WriteBusy) => {
            crate::http::store_wait::busy_response()
        }
        RootOperationRetryError::Store(error) => {
            tracing::error!(%error, "failed Library root plan correction rejected");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            )
        }
    }
}
