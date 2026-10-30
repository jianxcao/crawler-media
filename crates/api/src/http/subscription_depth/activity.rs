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
        queue.jobs_for_payload(&format!("{{\"subscribe_id\":\"{}\"}}", subscribe.id), limit)
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
    let created = store
        .subscribe_times(subscribe.id)
        .map(|(created, _)| created)
        .unwrap_or_default();
    drop(store);
    let fmt = crate::store::rfc3339_from_secs;
    let mut items: Vec<Value> = jobs
        .iter()
        .map(|job| {
            let status = job.status.as_str();
            let (activity_type, message) = match (job.kind.as_str(), status) {
                ("subscribe_search" | "subscribe_rss", "succeeded") => {
                    ("searched", "搜索完成，命中的资源已按规则评估")
                }
                ("transfer", "succeeded") => ("imported", "已入库完成"),
                ("transfer", "failed") => ("import_failed", "入库失败"),
                (kind, "failed") => (
                    "searched",
                    &format!(
                        "{kind} 失败：{}",
                        job.error.as_deref().unwrap_or("未知错误")
                    )[..],
                ),
                (kind, other) => (kind, &format!("{kind} {other}")[..]),
            };
            json!({
                "id": job.id.to_string(),
                "type": activity_type,
                "message": message,
                "kind": job.kind.as_str(),
                "status": status,
                "started_at": job.started_at.map(fmt),
                "finished_at": job.finished_at.map(fmt),
                "error": job.error,
                "wanted_item_id": Value::Null,
            })
        })
        .collect();
    // 最近一次入库事实也作为一条 imported 活动（facts 是权威来源，job 记录可能缺）。
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
    let total = items.len();
    ok(json!({ "items": items, "total": total })).into_response()
}
