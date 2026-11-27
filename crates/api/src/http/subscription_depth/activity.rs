use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::ok;
use crate::management::ApiState;

use crate::http::subscriptions::resolve_subscribe;

pub(crate) async fn activities(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let worker = state.clone();
    match state
        .http_work
        .run(crate::http::library::BROWSE_DEADLINE, move || {
            activities_blocking(worker, user_id, id, query)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => crate::http::library::browse_work_error(error),
    }
}

fn sanitize_for_role(raw: &str, is_admin: bool) -> String {
    if is_admin {
        hooks::sanitize_reason(raw)
    } else {
        hooks::redact_urls(raw)
    }
}

fn job_activity_item(job: &jobs::Job, is_admin: bool, fmt: fn(i64) -> String) -> Value {
    let status = job.status.as_str();
    let sanitized_error = job.error.as_deref().map(|e| sanitize_for_role(e, is_admin));
    let (activity_type, message) = match (job.kind.as_str(), status) {
        ("subscribe_search" | "subscribe_rss", "succeeded") => {
            ("searched", "搜索完成，命中的资源已按规则评估".to_string())
        }
        ("transfer", "succeeded") => ("imported", "已入库完成".to_string()),
        ("transfer", "failed") => ("import_failed", "入库失败".to_string()),
        (kind, "failed") => (
            "searched",
            format!(
                "{kind} 失败：{}",
                sanitized_error.as_deref().unwrap_or("未知错误")
            ),
        ),
        (kind, other) => (kind, format!("{kind} {other}")),
    };
    json!({
        "id": job.id.to_string(),
        "type": activity_type,
        "message": message,
        "kind": job.kind.as_str(),
        "status": status,
        "started_at": job.started_at.map(fmt),
        "finished_at": job.finished_at.map(fmt),
        "error": sanitized_error,
        "wanted_item_id": Value::Null,
    })
}

fn redact_enclosure_for_admin(raw: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(raw) {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        parsed.set_query(None);
        parsed.set_fragment(None);
        return hooks::sanitize_reason(parsed.as_str());
    }
    hooks::sanitize_reason(raw)
}

fn append_plugin_activities(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    limit: usize,
    is_admin: bool,
    fmt: fn(i64) -> String,
    items: &mut Vec<Value>,
) {
    let defers = state
        .store
        .list_subscribe_defers(subscribe_id, limit)
        .unwrap_or_default();
    let plugin_traces = state.plugins.list_subscribe_traces(subscribe_id);
    for defer in defers {
        let enclosure = if is_admin {
            json!(redact_enclosure_for_admin(&defer.enclosure))
        } else {
            Value::Null
        };
        let reason_text = sanitize_for_role(&defer.reason, is_admin);
        items.push(json!({
            "id": format!("defer-{}", defer.defer_id),
            "type": "deferred_candidate",
            "message": format!("{} ({}): {}", defer.hook_id, defer.state.as_str(), reason_text),
            "kind": defer.hook_id,
            "status": defer.state.as_str(),
            "started_at": fmt(defer.created_at),
            "finished_at": fmt(defer.updated_at),
            "error": Value::Null,
            "wanted_item_id": Value::Null,
            "attempt_count": defer.attempt_count,
            "next_retry_at": fmt(defer.next_retry_at),
            "enclosure": enclosure,
        }));
    }
    for mut trace in plugin_traces {
        let sanitized_reason = trace
            .reason
            .as_deref()
            .map(|r| sanitize_for_role(r, is_admin));
        trace.reason = sanitized_reason.clone();
        for step in &mut trace.steps {
            step.reason = step
                .reason
                .as_deref()
                .map(|r| sanitize_for_role(r, is_admin));
        }
        items.push(json!({
            "id": trace.id,
            "type": "hook_trace",
            "message": format!("{} -> {}", trace.hook_id, trace.decision),
            "kind": trace.hook_id,
            "status": trace.decision,
            "started_at": Value::Null,
            "finished_at": Value::Null,
            "error": sanitized_reason,
            "wanted_item_id": Value::Null,
            "plugin_trace": trace,
        }));
    }
}

fn activities_blocking(
    state: ApiState,
    user_id: domain::UserId,
    id: String,
    query: HashMap<String, String>,
) -> Response {
    let (subscribe, _media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let jobs = match crate::jobs_readers::read(&state.jobs_readers, |queue| {
        queue.jobs_for_identity(jobs::JobIdentity::subscribe_search(subscribe.id), limit)
    }) {
        Ok(jobs) => jobs,
        Err(error) => {
            tracing::warn!(%error, subscribe_id=%subscribe.id, "subscription activity query failed");
            return crate::library_read_work::stage_response(error);
        }
    };
    let store = state.store.clone();
    let facts_count = store
        .load_subscribe_facts(subscribe.id)
        .unwrap_or_default()
        .entries()
        .count();
    let created = match store
        .subscribe_times(subscribe.id)
        .and_then(|(created, _)| crate::http::subscriptions::timestamps::format(created))
    {
        Ok(created) => created,
        Err(error) => {
            tracing::error!(%error, subscribe_id=%subscribe.id, "Subscribe activity timestamp read failed");
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    drop(store);
    let is_admin = state
        .store
        .get_user(user_id)
        .ok()
        .flatten()
        .is_some_and(|u| u.role == domain::UserRole::Admin);
    let fmt = crate::store::rfc3339_from_secs;
    let mut items: Vec<Value> = jobs
        .iter()
        .map(|job| job_activity_item(job, is_admin, fmt))
        .collect();
    if facts_count > 0 {
        items.push(json!({
            "id": format!("facts-{}", subscribe.id),
            "type": "imported",
            "message": format!("已入库 {facts_count} 个单元（facts 汇总）"),
            "kind": "subscribe_facts",
            "status": "succeeded",
            "started_at": created,
            "finished_at": created,
            "error": Value::Null,
            "wanted_item_id": Value::Null,
        }));
    }
    append_plugin_activities(&state, subscribe.id, limit, is_admin, fmt, &mut items);
    let total = items.len();
    ok(json!({ "items": items, "total": total })).into_response()
}
