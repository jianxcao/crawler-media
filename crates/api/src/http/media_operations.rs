//! 合并操作：把两个 Media 的同一作品合并成一条 canonical 身份。
//!
//! `POST /media/merges` 只接受请求（写前预检与双方冻结已在 `Store` 内完成），
//! `GET /media/operations/{id}` 读回持久阶段，因此调用方可以用同一个
//! `operation_id` 轮询到 `succeeded`/`failed`，不需要重发请求。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::StoreError;

/// 预检/阶段失败 → HTTP。错误码区分「请求本身非法」「与既有状态冲突」
/// 「被冻结或缺少决议」，调用方据此决定改参数还是等待。
fn store_error(error: StoreError) -> Response {
    match error {
        StoreError::Invalid(message) => {
            tracing::warn!(%message, "Media merge request rejected as invalid");
            err(StatusCode::BAD_REQUEST, "media.merge_invalid", &message)
        }
        StoreError::Conflict(message) => {
            tracing::warn!(%message, "Media merge request conflicts with an existing operation");
            err(StatusCode::CONFLICT, "media.merge_conflict", &message)
        }
        StoreError::Protected(message) => {
            tracing::warn!(%message, "Media merge request blocked by identity state");
            err(StatusCode::CONFLICT, "media.merge_protected", &message)
        }
        StoreError::Missing(message) => err(StatusCode::NOT_FOUND, "media.merge_missing", &message),
        other => {
            tracing::error!(error = %other, "Media merge operation failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "media.merge_failed",
                &other.to_string(),
            )
        }
    }
}

fn required_str(body: &Value, key: &str) -> Result<String, Response> {
    body[key].as_str().map(str::to_string).ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "media.merge_invalid",
            &format!("缺少 {key}"),
        )
    })
}

fn required_i64(body: &Value, key: &str) -> Result<i64, Response> {
    body[key].as_i64().ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "media.merge_invalid",
            &format!("缺少 {key}"),
        )
    })
}

fn refresh_targets(body: &Value) -> Result<BTreeMap<String, String>, Response> {
    let Some(raw) = body.get("refresh_targets") else {
        return Ok(BTreeMap::new());
    };
    let map = raw.as_object().ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "media.merge_invalid",
            "refresh_targets 必须是对象",
        )
    })?;
    let mut out = BTreeMap::new();
    for (source, value) in map {
        let Some(external) = value.as_str() else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "media.merge_invalid",
                &format!("refresh_targets[{source}] 必须是字符串"),
            ));
        };
        out.insert(source.clone(), external.to_string());
    }
    Ok(out)
}

/// 一个 canonical 的 request_id 同时是 operation_id：调用方重发同一个请求
/// 得到同一个操作，不会产生第二份冻结。
fn merge_request(body: &Value) -> Result<crate::store::playback::MediaMergeRequest, Response> {
    let request_id = required_str(body, "request_id")?;
    let survivor = required_str(body, "survivor")?;
    let loser = required_str(body, "loser")?;
    let survivor = MediaId::from_str(&survivor).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "media.merge_invalid",
            "survivor 不是合法 MediaId",
        )
    })?;
    let loser = MediaId::from_str(&loser).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "media.merge_invalid",
            "loser 不是合法 MediaId",
        )
    })?;
    Ok(crate::store::playback::MediaMergeRequest {
        request_id,
        survivor,
        loser,
        survivor_revision: required_i64(body, "survivor_revision")?,
        loser_revision: required_i64(body, "loser_revision")?,
        refresh_targets: refresh_targets(body)?,
    })
}

/// POST /media/merges — 接受一次合并请求。202 返回 `operation_id`（= request_id），
/// 阶段推进由运行时驱动，调用方用 GET 轮询。
pub(crate) async fn post_merge(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let request = match merge_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let now_ms = state.playback_clock.now_ms();
    let survivor = request.survivor;
    let loser = request.loser;
    let worker = state.clone();
    let started = match state
        .http_work
        .run_to_completion(move || {
            super::store_wait::with_foreground_store(&worker, |store| {
                store.begin_media_merge(&request, now_ms)
            })
        })
        .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(response)) => return response,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return super::store_wait::busy_response();
        }
        Err(error) => {
            tracing::error!(%error, %survivor, %loser, "Media merge blocking worker failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "合并请求执行失败",
            );
        }
    };
    match started {
        Ok(operation_id) => {
            tracing::info!(
                %operation_id,
                %survivor,
                %loser,
                "Media merge accepted, both identities frozen"
            );
            (
                StatusCode::ACCEPTED,
                ok(json!({ "operation_id": operation_id })),
            )
                .into_response()
        }
        Err(error) => store_error(error),
    }
}

/// GET /media/operations/{id} — 读回合并操作的持久阶段，不推进它。
pub(crate) async fn get_operation(
    State(state): State<ApiState>,
    Path(operation_id): Path<String>,
) -> Response {
    let status = match super::store_wait::store_read(&state, move |store| {
        store.media_operation_status(&operation_id)
    })
    .await
    {
        Ok(status) => status,
        Err(error) => return error.response(),
    };
    match status {
        Ok(status) => ok(json!({
            "operation_id": status.operation_id,
            "phase": status.phase,
            "state": status.state,
            "error": status.error,
        }))
        .into_response(),
        Err(StoreError::Missing(_)) => err(
            StatusCode::NOT_FOUND,
            "media.operation_missing",
            "合并操作不存在",
        ),
        Err(other) => store_error(other),
    }
}
