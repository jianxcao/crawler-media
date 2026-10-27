//! Off-executor `Store` access for latency-sensitive handlers (Review L2).
//!
//! A handler must not take the process-wide `Store` lock on a tokio worker. A long
//! batch (scan, integrity, DRM probe) holds that mutex for seconds; waiting for it
//! parks the *worker*, so every other task scheduled on the runtime is delayed —
//! including requests that never touch the store. The lock is taken on the blocking
//! pool with a bounded wait instead, and a lock that stays contended is reported as
//! "busy, retry" rather than as a failure of the request.
//!
//! This is the Task 7 seam (`http/auth.rs::auth_store_lookup`, `http/jobs*::with_bounded_store`)
//! generalised into one place: blocking pool + bounded `try_lock_for` + `runtime.busy`.
//! Do not grow another flavour per handler.
//!
//! Two entry points, one taxonomy, **two budgets split by direction**:
//! - [`store_read`] for a handler that is already `async`: the whole store body runs
//!   on the blocking pool, waiting up to [`STORE_READ_WAIT`] (5 s).
//! - [`with_bounded_store`] for work that already runs inside a blocking budget
//!   (`state.http_work.run*`): the synchronous bounded wait for code that is off the
//!   executor already, waiting up to [`STORE_WRITE_WAIT`] (1 s).
//!
//! The asymmetry is deliberate (Task 27). A reader that waits 3 s and then succeeds
//! beats a 503 for a fire-and-forget heartbeat by a wide margin — a late read is
//! still a correct read, and a scan batch (~1.3 s for 100 files, ~950 ms of that is
//! publishing) is simply longer than any budget that keeps the old latency shape. A
//! writer is different: waiting 5 s and then answering `Deadline` has already spent
//! the user's patience on an outcome it cannot describe ("unknown", not "not
//! applied"), so the write path says "busy, retry" early and lets the client decide.

use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;

use crate::management::ApiState;

use super::err;

/// 读路径的等锁预算。
///
/// 健康读是几十微秒；整整 5 s 只会在长批持锁时用掉（扫描一批 ≈1.3 s，其中约
/// 950 ms 是发布成本）。读没有副作用：晚到的答案仍然是对的答案，所以宁可等。
pub(crate) const STORE_READ_WAIT: Duration = Duration::from_secs(5);

/// 写路径的等锁预算，刻意比读短。
///
/// 写等满 5 s 再报 [`StoreCallError::Deadline`]，等于花掉用户的耐心却仍无法描述
/// 结果——`Deadline` 的语义是「结果未知，可能已经写好」，不是「没写」。写侧宁可
/// 早说「忙，重试」（503 + `Retry-After`），由客户端决定是否重发。
pub(crate) const STORE_WRITE_WAIT: Duration = Duration::from_secs(1);

/// 内层 `try_lock_for` 比外层 timeout 早放弃的余量。
///
/// 两者取同一个时长时，「等锁等满」与「worker 超时」会在同一瞬间竞争，同一次争用
/// 既可能报 `Busy` 也可能报 `Deadline`。留出一条缝：锁等不到 → 确定地报 `Busy`，
/// `Deadline` 只留给「拿到锁之后 worker 仍未返回」。
const LOCK_SLACK: Duration = Duration::from_millis(50);

/// Why a bounded `Store` call produced no value.
#[derive(Debug)]
pub(crate) enum StoreCallError {
    /// The lock stayed contended for the whole budget ([`STORE_READ_WAIT`] /
    /// [`STORE_WRITE_WAIT`]).
    Busy,
    /// The blocking worker did not finish inside the deadline. A write may still
    /// land: the caller must treat this as "outcome unknown", not "not applied".
    Deadline,
    /// The blocking worker itself failed (panic / join error).
    Worker(String),
}

impl std::fmt::Display for StoreCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("store lock busy"),
            Self::Deadline => formatter.write_str("store call deadline"),
            Self::Worker(message) => write!(formatter, "store worker failed: {message}"),
        }
    }
}

impl StoreCallError {
    /// Contended lock → 503 + `Retry-After` (same code as identity reads and the
    /// write gate). A worker failure is a real server error, not backpressure.
    pub(crate) fn response(self) -> Response {
        match self {
            Self::Busy | Self::Deadline => busy_response(),
            Self::Worker(message) => {
                tracing::error!(%message, "Store 读的 blocking worker 失败");
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "runtime.worker",
                    "读取失败",
                )
            }
        }
    }
}

/// 503 + `Retry-After: 1`: the service is busy, the request is not wrong.
pub(crate) fn busy_response() -> Response {
    let mut response = err(
        StatusCode::SERVICE_UNAVAILABLE,
        "runtime.busy",
        "服务繁忙，请稍后重试",
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// Run `work(&Store)` on the blocking pool with a bounded wait for the lock.
///
/// `work` returns whatever the handler needs — usually `Result<T, Response>` so an
/// inner read failure keeps its own code. Lock contention and worker failure are
/// reported here, never as the handler's domain error.
pub(crate) async fn store_read<T, F>(state: &ApiState, work: F) -> Result<T, StoreCallError>
where
    F: FnOnce(&crate::Store) -> T + Send + 'static,
    T: Send + 'static,
{
    store_read_handle(state.store(), STORE_READ_WAIT, work).await
}

/// [`store_read`] on a bare `Store` handle, for callers (and tests) that hold the
/// handle rather than the whole `ApiState`.
pub(crate) async fn store_read_handle<T, F>(
    store: std::sync::Arc<parking_lot::Mutex<crate::Store>>,
    deadline: Duration,
    work: F,
) -> Result<T, StoreCallError>
where
    F: FnOnce(&crate::Store) -> T + Send + 'static,
    T: Send + 'static,
{
    // 锁等 deadline - slack：这样「锁拿不到」总是先于外层 timeout 报 Busy，
    // `Deadline` 只描述「拿到锁之后 worker 超过了预算」。
    let lock_wait = deadline.saturating_sub(LOCK_SLACK);
    let task = tokio::task::spawn_blocking(move || match store.try_lock_for(lock_wait) {
        Some(guard) => Ok(work(&guard)),
        None => Err(StoreCallError::Busy),
    });
    match tokio::time::timeout(deadline, task).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => {
            tracing::warn!(
                wait_ms = lock_wait.as_millis() as u64,
                "Store 锁未在预算内拿到，请求按繁忙重试"
            );
            Err(error)
        }
        Ok(Err(error)) => Err(StoreCallError::Worker(error.to_string())),
        Err(_) => {
            tracing::warn!(
                wait_ms = deadline.as_millis() as u64,
                "Store 调用超过预算，请求按繁忙重试"
            );
            Err(StoreCallError::Deadline)
        }
    }
}

/// Run `work(&Store)` on the blocking pool, waiting for the lock as long as it takes.
///
/// Background drivers (scan rounds, Library removal) have no client waiting on their
/// latency, so they may wait; what they must not do is park a runtime worker on the
/// process-wide mutex. A worker failure is reported, the lock wait is not a `Busy`.
pub(crate) async fn store_blocking<T, F>(
    store: std::sync::Arc<parking_lot::Mutex<crate::Store>>,
    work: F,
) -> Result<T, StoreCallError>
where
    F: FnOnce(&crate::Store) -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let guard = store.lock();
        work(&guard)
    })
    .await
    .map_err(|error| StoreCallError::Worker(error.to_string()))
}

/// Bounded `Store` lock for code that is already off the executor.
///
/// `None` means the lock stayed contended for [`STORE_WRITE_WAIT`]; the caller answers
/// 503 via [`busy_response`]. Use this from `state.http_work.run*` / `search_work.run`
/// closures only — calling it on a tokio worker reintroduces the stall this module
/// exists to remove.
///
/// 写侧用更短的预算：这里的工作通常带写，等到 `Deadline` 时结果已经无法描述
/// （见 [`StoreCallError::Deadline`]），所以早退早重试。
pub(crate) fn with_bounded_store<T>(
    store: std::sync::Arc<parking_lot::Mutex<crate::Store>>,
    work: impl FnOnce(&crate::Store) -> T,
) -> Option<T> {
    let guard = store.try_lock_for(STORE_WRITE_WAIT)?;
    Some(work(&guard))
}

#[cfg(test)]
mod tests;
