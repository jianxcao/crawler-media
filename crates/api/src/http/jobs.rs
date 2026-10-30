//! Jobs write/queue endpoints: manual run/cancel/tick, per-def toggle and schedule.
//!
//! Reading a snapshot lives in [`super::jobs_read`]; both halves share the blocking
//! budget helpers below (Review L2/L4).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use domain::JobDefId;
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;
use std::time::Duration;

use crate::blocking_budget::BlockingError;
use crate::http::{err, ok};
use crate::management::ApiState;

/// 任务端点（列表 / 详情 / 手动操作）的 blocking 预算。
pub(super) const JOBS_READ_DEADLINE: Duration = Duration::from_secs(2);
/// blocking 预算打满 / 等待超时：读端点用 503 + 重试表达。
pub(super) fn jobs_budget_response(error: BlockingError) -> Response {
    match error {
        BlockingError::Busy | BlockingError::Timeout => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "任务信息繁忙，请稍后重试",
        ),
        error => {
            tracing::error!(%error, "任务读端点 blocking worker 失败");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                &error.to_string(),
            )
        }
    }
}

pub(super) fn jobs_read_error(error: store::LibraryReadError) -> Response {
    let (status, code, message) = jobs_read_error_parts(error);
    err(status, code, &message)
}

pub(super) fn jobs_read_error_parts(
    error: store::LibraryReadError,
) -> (StatusCode, &'static str, String) {
    match error {
        store::LibraryReadError::Busy
        | store::LibraryReadError::Deadline
        | store::LibraryReadError::Cancelled
        | store::LibraryReadError::Closed => (
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "任务信息繁忙，请稍后重试".to_owned(),
        ),
        error => {
            tracing::error!(%error, "Jobs 只读快照失败");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                error.to_string(),
            )
        }
    }
}

/// Bounded snapshot read from the shared read-only Store pool.
pub(super) fn with_bounded_store<T>(
    state: &ApiState,
    work: impl FnOnce(&crate::Store) -> T,
) -> Result<T, BlockingError> {
    state
        .store
        .with_read_scope(JOBS_READ_DEADLINE, || work(&state.store))
        .map_err(|error| match error {
            store::StoreError::ReadBusy | store::StoreError::ReadClosed => BlockingError::Busy,
            store::StoreError::ReadDeadline | store::StoreError::ReadCancelled => {
                BlockingError::Timeout
            }
            error => BlockingError::Worker(error.to_string()),
        })
}

pub(crate) async fn run_job(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    // 有副作用（入队）：跑完再响应，绝不在执行器上等 Queue 的 SQLite 锁。
    let work_state = state.clone();
    match state
        .http_work
        .run_to_completion(move || run_job_blocking(&work_state, id))
        .await
    {
        Ok(response) => response,
        Err(error) => jobs_budget_response(error),
    }
}

fn run_job_blocking(state: &ApiState, id: JobDefId) -> Response {
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
    if !def.enabled {
        // 禁用任务不入队：显式 409，而不是 queued:true 却没入队（DB8 4.2）。
        return err(
            StatusCode::CONFLICT,
            "job.disabled",
            "任务已禁用，请先启用再手动执行",
        );
    }
    if let Some(response) = maintenance_already_running(&queue, &def) {
        return response;
    }
    let before = queue.get_live_child(def.id).ok().flatten();
    let scheduled = queue.ensure_scheduled_for(0, def.id);
    match scheduled {
        Ok(created_jobs) => ok(manual_run_body(before.as_ref(), &created_jobs)).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.enqueue_failed",
            &error.to_string(),
        ),
    }
}

/// 维护任务正在跑时直接返回；读失败不能当成没在跑。普通任务返回 `None`。
fn maintenance_already_running(queue: &jobs::Queue, def: &jobs::JobDef) -> Option<Response> {
    if !def.kind.is_maintenance() {
        return None;
    }
    match queue.has_running_child(def.id) {
        Ok(false) => None,
        Ok(true) => Some(
            ok(json!({
                "status": "already_running",
                "queued": false,
                "live_status": "running"
            }))
            .into_response(),
        ),
        Err(error) => Some(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.enqueue_failed",
            &error.to_string(),
        )),
    }
}

fn manual_run_body(before: Option<&jobs::Job>, created: &[jobs::Job]) -> Value {
    if !created.is_empty() {
        return json!({ "status": "created", "queued": true, "live_status": "queued" });
    }
    let Some(before) = before else {
        return json!({ "status": "already_running", "queued": false });
    };
    if before.status == jobs::JobStatus::Queued && before.run_after > 0 {
        json!({ "status": "expedited", "queued": true, "live_status": "queued" })
    } else {
        json!({
            "status": "already_queued",
            "queued": false,
            "live_status": before.status.as_str()
        })
    }
}

#[derive(Deserialize)]
pub(crate) struct ToggleJobInput {
    pub enabled: bool,
}

pub(crate) async fn toggle_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<ToggleJobInput>,
) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    // Review L2: Queue 是 jobs.db 上的阻塞写，挪到 blocking 池执行。
    let work_state = state.clone();
    let enabled = body.enabled;
    match state
        .http_work
        .run_to_completion(move || toggle_job_blocking(&work_state, id, enabled))
        .await
    {
        Ok(response) => response,
        Err(error) => jobs_budget_response(error),
    }
}

fn toggle_job_blocking(state: &ApiState, id: JobDefId, enabled: bool) -> Response {
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
    if let Err(error) = queue.set_def_enabled_by_id(def.id, enabled) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.toggle_failed",
            &error.to_string(),
        );
    }
    if !enabled {
        let _ = queue.cancel_def_children(def.id);
    }
    ok(json!({ "id": def.id.to_string(), "enabled": enabled })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct UpdateJobScheduleInput {
    pub interval_secs: Option<u64>,
}

pub(crate) async fn update_job_schedule(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateJobScheduleInput>,
) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    // Review L2: 改周期要写 jobs.db 并重排子任务，同样在 blocking 池上跑。
    let work_state = state.clone();
    let interval_secs = body.interval_secs;
    match state
        .http_work
        .run_to_completion(move || update_job_schedule_blocking(&work_state, id, interval_secs))
        .await
    {
        Ok(response) => response,
        Err(error) => jobs_budget_response(error),
    }
}

fn update_job_schedule_blocking(
    state: &ApiState,
    id: JobDefId,
    interval_secs: Option<u64>,
) -> Response {
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

    let new_schedule = match interval_secs {
        Some(secs) => {
            if secs == 0 {
                return err(
                    StatusCode::BAD_REQUEST,
                    "job.invalid_interval",
                    "执行周期必须大于 0 秒",
                );
            }
            // 维护任务不允许通过旧的 schedule 接口更新周期，必须走全配置原子的 maintenance-config 接口
            // 避免并发绕过 CAS 并产生不一致的调度覆盖。
            if def.kind.is_maintenance() {
                return err(
                    StatusCode::CONFLICT,
                    "job.use_maintenance_config",
                    "维护任务必须通过 /api/v1/jobs/{id}/maintenance-config 接口更新周期与配置",
                );
            }
            Some(jobs::Schedule::Interval { secs })
        }
        None => None,
    };

    if let Err(error) = queue.set_def_schedule_by_id(def.id, new_schedule) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.schedule_update_failed",
            &error.to_string(),
        );
    }

    // 取消由于旧调度计划在排队中的子任务，并按新 schedule 重新安排下次执行
    let _ = queue.cancel_def_children(def.id);
    let now = crate::job_loop::unix_now();
    let _ = queue.ensure_scheduled(now);

    ok(json!({
        "id": def.id.to_string(),
        "schedule": interval_secs.map(|secs| json!({ "interval_secs": secs }))
    }))
    .into_response()
}

pub(crate) async fn cancel_job(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    // Review L2: 取消失败要让调用方看见真实原因，阻塞段放 blocking 池。
    let work_state = state.clone();
    match state
        .http_work
        .run_to_completion(move || cancel_job_blocking(&work_state, id))
        .await
    {
        Ok(response) => response,
        Err(error) => jobs_budget_response(error),
    }
}

fn cancel_job_blocking(state: &ApiState, id: JobDefId) -> Response {
    let queue = state.jobs.lock();
    match queue.cancel_def_children(id) {
        Ok(count) => ok(json!({ "cancelled": count })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.cancel_failed",
            &error.to_string(),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct TickQuery {
    now: Option<i64>,
}

pub(crate) async fn tick_jobs(
    State(state): State<ApiState>,
    Query(query): Query<TickQuery>,
) -> Response {
    let requested_now = query.now;
    let now = requested_now.unwrap_or_else(crate::job_loop::unix_now);
    let outcome = tokio::task::spawn_blocking(move || {
        crate::job_loop::run_pending_jobs_with_clock(
            &state,
            now,
            || requested_now.unwrap_or_else(crate::job_loop::unix_now),
            crate::job_loop::TickSource::DevHttp,
        )
    })
    .await;
    match outcome {
        Ok(Ok(run)) => ok(json!({
            "ran": run.finished.len(),
            "kinds": run.finished.iter().map(|job| job.kind.as_str()).collect::<Vec<_>>(),
            // 维护任务不在这个开发入口执行（它需要自己的 Queue 短锁）；这里显式报告
            // 本轮被跳过的维护 kind，交给后台 job_loop。
            "skipped_maintenance": run.skipped_maintenance,
        }))
        .into_response(),
        Ok(Err(error)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.tick_failed",
            &error.to_string(),
        ),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.tick_failed",
            &error.to_string(),
        ),
    }
}

/// SSE stream the activity view subscribes to: emits `ready` on connect and a
/// `job` event whenever the shared count changes.
///
/// 所有连接共用 `JobUpdates` 的单个 2 秒 poller；客户端断开即结束本连接任务。
pub(crate) async fn job_stream(State(state): State<ApiState>) -> Response {
    use tokio_stream::wrappers::ReceiverStream;
    let mut updates = match state
        .job_updates
        .subscribe(state.jobs_readers.clone())
        .await
    {
        Ok(receiver) => receiver,
        Err(error) => {
            tracing::error!(%error, "任务计数订阅失败");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error);
        }
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        if tx
            .send(Ok(Event::default().event("ready").data("{}")))
            .await
            .is_err()
        {
            return;
        }

        // 订阅时已持有真实当前计数：立即下发，不空等 2 秒。
        let mut last = (*updates.borrow()).clone();
        if tx
            .send(Ok(Event::default()
                .event("job")
                .data(format!(r#"{{"count":{}}}"#, last.count))))
            .await
            .is_err()
        {
            return;
        }

        loop {
            tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let current = (*updates.borrow()).clone();
                    if current == last {
                        continue;
                    }
                    if tx
                        .send(Ok(Event::default()
                            .event("job")
                            .data(format!(r#"{{"count":{}}}"#, current.count))))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    last = current;
                }
                _ = tx.closed() => break,
            }
        }
    });
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}
