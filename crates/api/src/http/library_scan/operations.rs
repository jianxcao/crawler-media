//! HTTP handlers for scan operation polling and cancellation.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::http::{err, ok};
use crate::management::ApiState;

pub(crate) async fn get_scan_operation(
    State(state): State<ApiState>,
    Path((lib_id, op_id)): Path<(String, String)>,
) -> Response {
    // Polling is the hot path of every scan waiter. Taking the process-wide
    // Store lock on the async worker parks that worker for the whole wait, and
    // a scan that holds the lock (metadata, NFO) then stalls every other poll
    // on the same runtime — the waiter reports a timeout for a scan that
    // already finished. The read belongs on the blocking pool.
    let read = super::super::store_wait::store_read(&state, move |store| {
        store.scan_operation(&op_id).map(|found| (found, lib_id))
    })
    .await;
    let (found, lib_id) = match read {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        Err(error) => return error.response(),
    };
    match found {
        Some(op) if op.library_id == lib_id => ok(json!({
            "operation_id": op.id,
            "status": op.status.as_str(),
            "generation": op.generation,
            "report": op.report,
        }))
        .into_response(),
        Some(_) | None => err(
            StatusCode::NOT_FOUND,
            "scan_operation.missing",
            "扫描操作不存在",
        ),
    }
}

pub(crate) async fn cancel_scan_operation(
    State(state): State<ApiState>,
    Path((lib_id, op_id)): Path<(String, String)>,
) -> Response {
    let worker = state.clone();
    match state
        .http_work
        .run_to_completion(move || cancel_scan_blocking(&worker, lib_id, op_id))
        .await
    {
        Ok(response) => response,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            super::super::store_wait::busy_response()
        }
        Err(error) => super::super::library::browse_work_error(error),
    }
}

fn cancel_scan_blocking(state: &ApiState, lib_id: String, op_id: String) -> Response {
    let now_ms = crate::job_loop::unix_now() * 1000;
    let cancelled = super::super::store_wait::with_foreground_store(state, |store| {
        store.library_write_with_error(
            |error| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                )
            },
            |db| {
                let op = db
                    .scan_operation(&op_id)
                    .map_err(|error| {
                        err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "store.error",
                            &error.to_string(),
                        )
                    })?
                    .filter(|op| op.library_id == lib_id)
                    .ok_or_else(|| {
                        err(
                            StatusCode::NOT_FOUND,
                            "scan_operation.missing",
                            "未找到可取消的扫描操作",
                        )
                    })?;
                let cancelled = db.cancel_scan(&op_id, now_ms).map_err(|error| {
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    )
                })?;
                Ok((cancelled, op.status))
            },
        )
    });
    match cancelled {
        Ok(Ok((true, _))) => {
            state.scan_coordinator.cancel_operation(&op_id);
            ok(json!({ "cancelled": true })).into_response()
        }
        Ok(Ok((false, status))) => ok(json!({
            "cancelled": false,
            "status": status.as_str(),
        }))
        .into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}
