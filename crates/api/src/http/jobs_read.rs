//! Jobs read endpoints: defs with schedule, live/last children.
//!
//! Split out of `http::jobs` (the write/queue side) to keep both files well
//! under the 800-line limit: this module only renders snapshots, it never
//! mutates the queue.

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::JobDefId;
use domain::UserId;
use jobs::JobKind;
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use super::jobs::{JOBS_READ_DEADLINE, jobs_budget_response, with_bounded_store};
use crate::blocking_budget::BlockingError;
use crate::http::{err, ok, ok_list};
use crate::maintenance::MaintenancePolicy;
use crate::management::ApiState;

fn job_media_title(payload_val: &Value, store: &crate::Store) -> Option<String> {
    if let Some(sub_id_str) = payload_val.get("subscribe_id").and_then(|v| v.as_str()) {
        if let Ok(sub_id) = domain::SubscribeId::from_str(sub_id_str) {
            store
                .get_subscribe(sub_id)
                .ok()
                .flatten()
                .and_then(|sub| store.get_media(sub.media_id).ok().flatten())
                .map(|m| m.title)
        } else {
            None
        }
    } else if let Some(med_id_str) = payload_val.get("media_id").and_then(|v| v.as_str()) {
        if let Ok(med_id) = domain::MediaId::from_str(med_id_str) {
            store.get_media(med_id).ok().flatten().map(|m| m.title)
        } else {
            None
        }
    } else {
        None
    }
}

fn job_friendly_name(name: &str, media_title: Option<&str>) -> String {
    if name.starts_with("search ") || name.starts_with("catalog ") {
        if let Some(title) = media_title {
            if name.starts_with("search ") {
                format!("自动搜索《{title}》")
            } else {
                format!("刷新元数据《{title}》")
            }
        } else {
            name.to_string()
        }
    } else {
        match name {
            "RSS" => "站点 RSS 增量抓取".into(),
            "Transfer" => "媒体文件入库整理".into(),
            "Watch intake" => "监控目录变动吸纳入库".into(),
            "Scrape" => "元数据与封面刮削".into(),
            "Check-in" => "站点每日自动签到".into(),
            _ => name.to_string(),
        }
    }
}

fn job_last_status<'a>(
    live: Option<&'a jobs::Job>,
    last: Option<&'a jobs::Job>,
) -> Option<&'a str> {
    match (live, last) {
        (Some(l), _) if l.status == jobs::JobStatus::Running || l.run_after == 0 => {
            Some(l.status.as_str())
        }
        (_, Some(r)) => Some(r.status.as_str()),
        (Some(l), None) => Some(l.status.as_str()),
        (None, None) => None,
    }
}

/// 渲染 def 需要的 media title（payload → subscribe/media → store）。
fn def_media_title(def: &jobs::JobDef, store: &crate::Store) -> Option<String> {
    let payload_val: Value = serde_json::from_str(&def.payload).unwrap_or_else(|_| json!({}));
    job_media_title(&payload_val, store)
}

/// 维护 def 的策略摘要（typed，供前端参数抽屉直接渲染）。
///
/// 非维护 def 返回 `None`，调用方不写该键——既有消费方看到的 DTO 不变。
fn maintenance_summary(def: &jobs::JobDef) -> Option<Value> {
    if !def.kind.is_maintenance() {
        return None;
    }
    let summary = match MaintenancePolicy::parse(def.kind, &def.payload) {
        Ok(MaintenancePolicy::Database(policy)) => json!({
            "kind": def.kind.as_str(),
            "version": policy.version,
            "vacuum_enabled": policy.vacuum_enabled,
        }),
        Ok(MaintenancePolicy::Jobs(policy)) => json!({
            "kind": def.kind.as_str(),
            "version": policy.version,
            "retention_days": policy.retention_days,
            "dry_run": policy.dry_run,
        }),
        Ok(MaintenancePolicy::PlaybackHistory(policy)) => json!({
            "kind": def.kind.as_str(),
            "version": policy.version,
            "retention_days": policy.retention_days,
            "dry_run": policy.dry_run,
        }),
        Ok(MaintenancePolicy::DataRetention(policy)) => json!({
            "kind": def.kind.as_str(),
            "version": policy.version,
            "dry_run": policy.dry_run,
            "qoe_days": policy.qoe_days,
            "transient_cache_days": policy.transient_cache_days,
            "probe_days": policy.probe_days,
            "operations_days": policy.operations_days,
        }),
        Err(message) => {
            // payload 坏了要显式暴露，不能假装是默认策略。
            tracing::error!(
                job_def_id = %def.id,
                kind = def.kind.as_str(),
                %message,
                "维护 def 的 payload 无法解析"
            );
            json!({
                "kind": def.kind.as_str(),
                "invalid": true,
                "error": message,
            })
        }
    };
    Some(summary)
}

/// 最近一次维护运行报告（读 `job.progress` 的原始 JSON 字符串）。
///
/// 报告可能由终态 job 或仍在运行的 job 写入；优先取最近一次终态。
fn maintenance_report(live: Option<&jobs::Job>, last: Option<&jobs::Job>) -> Option<String> {
    last.and_then(|job| job.progress.clone())
        .or_else(|| live.and_then(|job| job.progress.clone()))
}

/// 纯渲染：调用方在锁外传入预先取好的 media title。
fn def_json(
    def: &jobs::JobDef,
    live: Option<&jobs::Job>,
    last: Option<&jobs::Job>,
    media_title: Option<&str>,
) -> Value {
    let payload_val: Value = serde_json::from_str(&def.payload).unwrap_or_else(|_| json!({}));
    let friendly_name = job_friendly_name(&def.name, media_title);

    let mut rendered = json!({
        "id": def.id.to_string(),
        "kind": def.kind.as_str(),
        "name": def.name,
        "friendly_name": friendly_name,
        "enabled": def.enabled,
        "schedule": match def.schedule {
            Some(jobs::Schedule::Interval { secs }) => json!({ "interval_secs": secs }),
            None => Value::Null,
        },
        "payload": payload_val,
        "concurrency_key": def.concurrency_key,
        "last_status": job_last_status(live, last),
        "last_finished_at": last.and_then(|job| job.finished_at).or_else(|| live.and_then(|job| job.finished_at)),
        "last_error": last.and_then(|job| job.error.as_deref()).or_else(|| live.and_then(|job| job.error.as_deref())),
        "next_run_after": if !def.enabled {
            Value::Null
        } else {
            live.map(|job| json!(job.run_after)).unwrap_or_else(|| {
                last.and_then(|job| job.finished_at)
                    .and_then(|finished| def.schedule.as_ref().map(|s| json!(s.next_after(finished))))
                    .unwrap_or(Value::Null)
            })
        },
    });
    // 维护 def 才附加 typed 摘要 / 最近报告 / 修改令牌；其他 def 的既有字段不变。
    if let Some(object) = rendered.as_object_mut() {
        if let Some(summary) = maintenance_summary(def) {
            object.insert("maintenance".into(), summary);
            // 修改令牌：下一次 CAS 回传这个值（不是 payload 内的 schema version）。
            object.insert("payload_token".into(), Value::String(def.payload.clone()));
        }
        if let Some(report) = maintenance_report(live, last) {
            object.insert("last_report".into(), Value::String(report));
        }
    }
    rendered
}

/// 单个 def 的渲染便捷入口（仍需 Store 取 media title）。
fn def_json_with_store(
    def: &jobs::JobDef,
    live: Option<&jobs::Job>,
    last: Option<&jobs::Job>,
    store: &crate::Store,
) -> Value {
    def_json(def, live, last, def_media_title(def, store).as_deref())
}

#[derive(Deserialize)]
pub(crate) struct ListJobsQuery {
    pub scope: Option<String>,
    pub active_only: Option<bool>,
    pub limit: Option<usize>,
}

fn def_visible_to_user(
    def: &jobs::JobDef,
    store: &crate::Store,
    user_id: UserId,
    admin: bool,
) -> bool {
    if admin || def.kind != JobKind::SubscribeSearch {
        return true;
    }
    let Some(raw_id) = serde_json::from_str::<Value>(&def.payload)
        .ok()
        .and_then(|payload| {
            payload
                .get("subscribe_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    else {
        return false;
    };
    let Ok(subscribe_id) = domain::SubscribeId::from_str(&raw_id) else {
        return false;
    };
    store
        .get_subscribe(subscribe_id)
        .ok()
        .flatten()
        .is_some_and(|subscribe| subscribe.user_id == user_id)
}

pub(crate) async fn list_jobs(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Query(query): Query<ListJobsQuery>,
) -> Response {
    // Review L2: Queue / Store 两把锁都是阻塞的，整个快照在 blocking 池上取。
    let work_state = state.clone();
    match state
        .http_work
        .run(JOBS_READ_DEADLINE, move || {
            list_jobs_blocking(&work_state, user_id, &query)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => jobs_budget_response(error),
    }
}

fn list_jobs_blocking(state: &ApiState, user_id: UserId, query: &ListJobsQuery) -> Response {
    // 1) Queue 快照：defs（单独加锁，不与 Store 嵌套）。
    let defs = match state.jobs.lock().list_defs() {
        Ok(defs) => defs,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    // 2) Store 快照：可见性与 scope 过滤（都在 limit 之前）。
    let visible_defs = match with_bounded_store(state, |store| visible_defs(store, defs, user_id, query)) {
        Some(visible_defs) => visible_defs,
        None => return jobs_budget_response(BlockingError::Busy),
    };
    // 3) Queue 快照：一次批量取所有 live/last child（不再逐 def 查询）。
    let children = {
        let queue = state.jobs.lock();
        let ids: Vec<JobDefId> = visible_defs.iter().map(|def| def.id).collect();
        queue.children_for_defs(&ids).unwrap_or_default()
    };
    let selected = select_rows(visible_defs, children, query);
    // 4) Store 快照：只取渲染所需的 media title，锁内不渲染 JSON。
    let titles = match with_bounded_store(state, |store| {
        selected
            .iter()
            .map(|(def, _, _)| def_media_title(def, store))
            .collect::<Vec<Option<String>>>()
    }) {
        Some(titles) => titles,
        None => return jobs_budget_response(BlockingError::Busy),
    };
    // 5) 锁外渲染。
    let listed: Vec<Value> = selected
        .into_iter()
        .zip(titles)
        .map(|((def, live, last), title)| {
            def_json(&def, live.as_ref(), last.as_ref(), title.as_deref())
        })
        .collect();
    ok_list(listed).into_response()
}

/// 可见性 + scope 过滤：锁内只读事实，不渲染。
fn visible_defs(
    store: &crate::Store,
    defs: Vec<jobs::JobDef>,
    user_id: UserId,
    query: &ListJobsQuery,
) -> Vec<jobs::JobDef> {
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    defs.into_iter()
        .filter(|def| {
            if !def_visible_to_user(def, store, user_id, admin) {
                return false;
            }
            // scope=system 仅保留系统级任务，排除条目级任务
            // （subscribe_search / catalog_refresh）。
            if query.scope.as_deref() == Some("system") {
                let is_individual = def.kind == JobKind::SubscribeSearch
                    || def.kind == JobKind::CatalogRefresh
                    || def.name.starts_with("search ")
                    || def.name.starts_with("catalog ");
                if is_individual {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// 把 live/last child 贴回 def，并按 `active_only` / `limit` 截断。
fn select_rows(
    visible_defs: Vec<jobs::JobDef>,
    children: Vec<jobs::JobChildren>,
    query: &ListJobsQuery,
) -> Vec<(jobs::JobDef, Option<jobs::Job>, Option<jobs::Job>)> {
    let mut by_def: std::collections::HashMap<JobDefId, jobs::JobChildren> = children
        .into_iter()
        .map(|child| (child.def_id, child))
        .collect();
    let mut selected = Vec::with_capacity(visible_defs.len());
    for def in visible_defs {
        let (live, last) = match by_def.remove(&def.id) {
            Some(child) => (child.live, child.last),
            None => (None, None),
        };
        if query.active_only == Some(true) && live.is_none() {
            continue;
        }
        selected.push((def, live, last));
        if let Some(limit) = query.limit
            && selected.len() >= limit
        {
            break;
        }
    }
    selected
}

pub(crate) async fn get_job(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    let work_state = state.clone();
    match state
        .http_work
        .run(JOBS_READ_DEADLINE, move || {
            get_job_blocking(&work_state, user_id, id)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => jobs_budget_response(error),
    }
}

fn get_job_blocking(state: &ApiState, user_id: UserId, id: JobDefId) -> Response {
    // Queue 快照：取完即放锁，不与 Store 锁嵌套（Store 锁是有界等待）。
    let (def, live, last) = {
        let queue = state.jobs.lock();
        let defs = match queue.list_defs() {
            Ok(defs) => defs,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        let Some(def) = defs.into_iter().find(|d| d.id == id) else {
            return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
        };
        let live = queue.get_live_child(def.id).ok().flatten();
        let last = queue.get_last_child(def.id).ok().flatten();
        (def, live, last)
    };
    let rendered = with_bounded_store(state, |store| {
        let admin = store
            .user_role(user_id)
            .map(|role| role == "admin")
            .unwrap_or(false);
        if !def_visible_to_user(&def, store, user_id, admin) {
            return None;
        }
        Some(def_json_with_store(
            &def,
            live.as_ref(),
            last.as_ref(),
            store,
        ))
    });
    match rendered {
        Some(Some(rendered)) => ok(rendered).into_response(),
        Some(None) => err(StatusCode::NOT_FOUND, "job.missing", "任务不存在"),
        None => jobs_budget_response(BlockingError::Busy),
    }
}
