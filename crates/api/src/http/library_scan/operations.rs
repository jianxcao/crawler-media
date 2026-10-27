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
    let now_ms = crate::job_loop::unix_now() * 1000;
    let cancel_res = {
        let store = state.store.lock();
        let op_opt = match store.scan_operation(&op_id) {
            Ok(opt) => opt,
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &e.to_string(),
                );
            }
        };
        let Some(op) = op_opt else {
            return err(
                StatusCode::NOT_FOUND,
                "scan_operation.missing",
                "未找到可取消的扫描操作",
            );
        };
        if op.library_id != lib_id {
            return err(
                StatusCode::NOT_FOUND,
                "scan_operation.missing",
                "未找到可取消的扫描操作",
            );
        }
        store.cancel_scan(&op_id, now_ms)
    };

    match cancel_res {
        Ok(true) => {
            state.scan_coordinator.cancel_operation(&op_id);
            ok(json!({ "cancelled": true })).into_response()
        }
        Ok(false) => {
            // 幂等：操作存在但已处于终态（completed/failed/cancelled）时不再改状态，
            // 也不该报 404 —— 重复取消同一个操作是合法请求。
            let store = state.store.lock();
            match store.scan_operation(&op_id) {
                Ok(Some(op)) if op.library_id == lib_id => ok(json!({
                    "cancelled": false,
                    "status": op.status.as_str(),
                }))
                .into_response(),
                Ok(_) => err(
                    StatusCode::NOT_FOUND,
                    "scan_operation.missing",
                    "未找到可取消的扫描操作",
                ),
                Err(e) => err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &e.to_string(),
                ),
            }
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &e.to_string(),
        ),
    }
}
