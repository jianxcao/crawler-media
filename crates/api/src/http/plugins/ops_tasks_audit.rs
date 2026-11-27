use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::UserId;
use uuid::Uuid;

use super::dto::{
    CancelOperationRequest, ResumeOperationRequest, RunTaskRequest, SubmitCommandRequest,
    UpdateTaskRequest,
};
use super::extractors::{
    CanonicalOptionalRevisionWrite, CanonicalWrite, convert_command_targets, parse_from_timestamp,
    parse_pagination_only, parse_query_pairs, parse_uuid_param, validate_cursor, validate_limit,
};
use super::{accepted_op, ok_json, ok_paged, ok_with_etag, service_error_response};
use crate::http::err;
use crate::management::ApiState;
use crate::plugins::{
    CancelOperationInput, ResumeOperationInput, RunTaskInput, SubmitAdminCommandInput,
    UpdateTaskInput,
};

pub(super) async fn get_operation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> Response {
    let op_id = match parse_uuid_param(&raw_id, "operation_id") {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let view = match state.plugins().get_operation(&state, op_id).await {
        Ok(v) => v,
        Err(err) => return service_error_response(err),
    };
    if crate::plugins::has_attribution_headers(&headers) {
        let Some(attr_pid) = view.plugin_id.as_deref() else {
            return service_error_response(crate::plugins::PluginServiceError::forbidden(
                "plugin.forbidden",
                "system operation is not accessible to plugin UI",
            ));
        };
        if let Err(err) = state
            .plugins()
            .verify_plugin_attribution(&state, attr_pid, &headers)
            .await
        {
            return service_error_response(err);
        }
    }
    ok_with_etag(StatusCode::OK, view.revision, &view)
}

pub(super) async fn cancel_operation(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(raw_id): Path<String>,
    w: CanonicalWrite<CancelOperationRequest>,
) -> Response {
    let op_id = match parse_uuid_param(&raw_id, "operation_id") {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match state
        .plugins()
        .cancel_operation(
            &state,
            op_id,
            CancelOperationInput {
                actor_user_id: user_id,
                reason: w.body.reason,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn resume_operation(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(raw_id): Path<String>,
    w: CanonicalWrite<ResumeOperationRequest>,
) -> Response {
    let op_id = match parse_uuid_param(&raw_id, "operation_id") {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match state
        .plugins()
        .resume_operation(
            &state,
            op_id,
            ResumeOperationInput {
                actor_user_id: user_id,
                expected_revision: w.expected_revision,
                evidence_ref: w.body.evidence_ref,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn list_tasks(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    if crate::plugins::has_attribution_headers(&headers)
        && let Err(err) = state
            .plugins()
            .verify_plugin_attribution(&state, &id, &headers)
            .await
    {
        return service_error_response(err);
    }
    let (cursor, limit) = match parse_pagination_only(raw_query.as_deref()) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match state.plugins().list_tasks(&state, &id, cursor, limit).await {
        Ok(page) => ok_paged(page),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn update_task(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
    w: CanonicalWrite<UpdateTaskRequest>,
) -> Response {
    if let Err(err) = state
        .plugins()
        .verify_plugin_attribution_with_cap(
            &state,
            &id,
            &headers,
            Some(&format!("task:run:{task_id}")),
        )
        .await
    {
        return service_error_response(err);
    }
    match state
        .plugins()
        .update_task(
            &state,
            &id,
            &task_id,
            UpdateTaskInput {
                actor_user_id: user_id,
                enabled: w.body.enabled,
                interval_secs: w.body.schedule.interval_secs,
                config: w.body.config,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn run_task(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
    w: CanonicalWrite<RunTaskRequest>,
) -> Response {
    if let Err(err) = state
        .plugins()
        .verify_plugin_attribution_with_cap(
            &state,
            &id,
            &headers,
            Some(&format!("task:run:{task_id}")),
        )
        .await
    {
        return service_error_response(err);
    }
    match state
        .plugins()
        .run_task(
            &state,
            &id,
            &task_id,
            RunTaskInput {
                actor_user_id: user_id,
                input: w.body.input,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn list_task_receipts(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let task_cap = format!("task:run:{task_id}");
    if crate::plugins::has_attribution_headers(&headers)
        && let Err(err) = state
            .plugins()
            .verify_plugin_attribution_with_any_cap(
                &state,
                &id,
                &headers,
                &["read:history", task_cap.as_str()],
            )
            .await
    {
        return service_error_response(err);
    }
    let mut cursor: Option<Uuid> = None;
    let mut limit: Option<usize> = None;
    for (k, v) in parse_query_pairs(raw_query.as_deref()) {
        match k.as_str() {
            "cursor" => {
                if v.is_empty() {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "plugin.invalid_input",
                        "cursor must not be empty",
                    );
                }
                match parse_uuid_param(&v, "cursor") {
                    Ok(u) => cursor = Some(u),
                    Err(resp) => return resp,
                }
            }
            "limit" => match validate_limit(&v) {
                Ok(l) => limit = Some(l),
                Err(resp) => return resp,
            },
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    &format!("unsupported query parameter: {k}"),
                );
            }
        }
    }
    match state
        .plugins()
        .list_task_receipts(&state, &id, &task_id, cursor, limit)
        .await
    {
        Ok(page) => ok_paged(page),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn submit_command(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path(id): Path<String>,
    w: CanonicalOptionalRevisionWrite<SubmitCommandRequest>,
) -> Response {
    let required_cap = format!("command:{}", w.body.kind);
    let (plugin, attributed_gen) = match state
        .plugins()
        .verify_plugin_attribution_with_cap(&state, &id, &headers, Some(&required_cap))
        .await
    {
        Ok(pair) => pair,
        Err(err) => return service_error_response(err),
    };
    if attributed_gen.is_some()
        && let Err(err) =
            require_attributed_command_handles(&w.body.kind, &w.body.payload, &w.body.targets)
    {
        return service_error_response(err);
    }
    let expected_revision = w.expected_revision.unwrap_or(plugin.revision);
    let targets = convert_command_targets(w.body.targets);
    match state
        .plugins()
        .submit_admin_command(
            &state,
            &id,
            SubmitAdminCommandInput {
                actor_user_id: user_id,
                kind: w.body.kind,
                payload: w.body.payload,
                targets,
                evidence_ref: w.body.evidence_ref,
                expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

fn non_empty_str(val: Option<&serde_json::Value>) -> bool {
    val.and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}

fn non_empty_str_array(val: Option<&serde_json::Value>) -> bool {
    val.and_then(|v| v.as_array()).is_some_and(|arr| {
        !arr.is_empty()
            && arr
                .iter()
                .all(|item| item.as_str().is_some_and(|s| !s.trim().is_empty()))
    })
}

fn require_attributed_command_handles(
    kind: &str,
    payload: &serde_json::Value,
    targets: &[super::dto::HttpCommandTarget],
) -> Result<(), crate::plugins::PluginServiceError> {
    if !targets.is_empty() {
        return Err(crate::plugins::PluginServiceError::invalid(
            "plugin.invalid_handle",
            "UI-attributed command forbids explicit raw targets; targets are derived from handles",
        ));
    }
    let obj = payload.as_object().ok_or_else(|| {
        crate::plugins::PluginServiceError::invalid(
            "plugin.invalid_input",
            "command payload must be an object",
        )
    })?;
    match kind {
        "site.maintenance" | "site.check-in" | "site.verify-session" => {
            if obj.contains_key("site_id") || !non_empty_str(obj.get("site_handle")) {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_handle",
                    "UI-attributed site command requires opaque site_handle",
                ));
            }
        }
        "downloader.add" => {
            return Err(crate::plugins::PluginServiceError::forbidden(
                "plugin.capability_denied",
                "UI surfaces cannot submit raw downloader.add commands directly",
            ));
        }
        "downloader.delete-task" | "downloader.control" => {
            if obj.contains_key("task_id")
                || obj.contains_key("task_fact_id")
                || obj.contains_key("task_handle")
                || obj.contains_key("info_hash")
                || obj.contains_key("hashes")
                || obj.contains_key("downloader_id")
                || !non_empty_str(obj.get("owned_task_handle"))
            {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_handle",
                    "UI-attributed downloader task command requires opaque owned_task_handle",
                ));
            }
        }
        "downloader.prepare-cleanup" => {
            if obj.contains_key("task_ids")
                || obj.contains_key("targets")
                || obj.contains_key("task_id")
                || obj.contains_key("downloader_id")
                || !non_empty_str_array(obj.get("owned_task_handles"))
            {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_handle",
                    "UI-attributed prepare-cleanup command requires opaque owned_task_handles",
                ));
            }
        }
        "boost.run" => {
            if obj.contains_key("site_id")
                || obj.contains_key("sites")
                || obj.contains_key("downloader_id")
            {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_handle",
                    "UI-attributed boost.run forbids raw site_id/sites/downloader_id",
                ));
            }
        }
        "indexer.search" => {
            if obj.contains_key("site_id") || obj.contains_key("site_ids") {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_handle",
                    "UI-attributed indexer.search forbids raw site_id/site_ids; use site_handle or sites",
                ));
            }
            if obj.contains_key("sites") && obj.contains_key("site_handle") {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_input",
                    "cannot provide both sites and site_handle",
                ));
            }
            if obj
                .get("sites")
                .and_then(|v| v.as_array())
                .is_some_and(Vec::is_empty)
            {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_input",
                    "sites must not be empty",
                ));
            }
            if !non_empty_str(obj.get("site_handle")) && !non_empty_str_array(obj.get("sites")) {
                return Err(crate::plugins::PluginServiceError::invalid(
                    "plugin.invalid_handle",
                    "UI-attributed indexer.search requires opaque site_handle or sites",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn list_audit(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let mut action: Option<String> = None;
    let mut from_ts: Option<i64> = None;
    let mut cursor: Option<String> = None;
    let mut limit: Option<usize> = None;
    for (k, v) in parse_query_pairs(raw_query.as_deref()) {
        match k.as_str() {
            "action" => {
                if v.is_empty() {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "plugin.invalid_input",
                        "action must not be empty",
                    );
                }
                action = Some(v);
            }
            "from" => match parse_from_timestamp(&v) {
                Ok(ts) => from_ts = Some(ts),
                Err(resp) => return resp,
            },
            "cursor" => match validate_cursor(v) {
                Ok(c) => cursor = Some(c),
                Err(resp) => return resp,
            },
            "limit" => match validate_limit(&v) {
                Ok(l) => limit = Some(l),
                Err(resp) => return resp,
            },
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    &format!("unsupported query parameter: {k}"),
                );
            }
        }
    }
    match state
        .plugins()
        .list_audit(&state, &id, action, from_ts, cursor, limit)
        .await
    {
        Ok(page) => ok_paged(page),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn get_metrics(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let mut window_secs: Option<u64> = None;
    for (k, v) in parse_query_pairs(raw_query.as_deref()) {
        match k.as_str() {
            "window_secs" => {
                let parsed = match v.parse::<u64>() {
                    Ok(n) if n > 0 => n,
                    _ => {
                        return err(
                            StatusCode::BAD_REQUEST,
                            "plugin.invalid_input",
                            "window_secs must be a positive integer",
                        );
                    }
                };
                window_secs = Some(parsed);
            }
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    &format!("unsupported query parameter: {k}"),
                );
            }
        }
    }
    match state.plugins().get_metrics(&state, &id, window_secs).await {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn get_hook_catalog() -> Response {
    let items = crate::plugins::hooks_runtime::hook_catalog_items();
    ok_json(
        StatusCode::OK,
        &serde_json::json!({
            "items": items,
            "total": items.len(),
        }),
    )
}
