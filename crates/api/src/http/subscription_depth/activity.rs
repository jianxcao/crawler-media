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
    let (subscribe, _media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let jobs = state
        .jobs
        .lock()
        .jobs_for_payload(&format!("{{\"subscribe_id\":\"{}\"}}", subscribe.id), limit);
    let store = state.store.lock();
    let facts = store.load_subscribe_facts(subscribe.id).unwrap_or_default();
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
    let created = store
        .subscribe_times(subscribe.id)
        .map(|(c, _)| c)
        .unwrap_or_default();
    let facts_count = facts.entries().count();
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
