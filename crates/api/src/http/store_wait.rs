//! Off-executor Store access with per-database reader and writer admission.
//!
//! Request reads use [`store_read`] and borrow only the database each SQL method
//! needs. Request writes use [`store_write`]; admission is checked again while
//! holding the actual database writer. Callers already on a blocking worker use
//! [`with_store_read`] or [`with_foreground_store`] for the same policy.

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

/// Reader admission for callers that already run on a blocking worker.
pub(crate) fn with_store_read<T>(
    state: &ApiState,
    work: impl FnOnce(&crate::Store) -> T,
) -> Result<T, Response> {
    state
        .store
        .with_read_snapshot(STORE_READ_WAIT, || work(&state.store))
        .map_err(|error| match error {
            store::StoreError::ReadBusy
            | store::StoreError::ReadClosed
            | store::StoreError::ReadDeadline
            | store::StoreError::ReadCancelled => busy_response(),
            error => {
                tracing::error!(%error, "Store reader scope failed");
                err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "读取失败")
            }
        })
}

/// A bounded off-executor mutation. Admission is rechecked after acquiring the
/// Store, so a request queued before maintenance/read-only cannot write afterward.
pub(crate) async fn store_write<T, F>(state: &ApiState, work: F) -> Result<T, Response>
where
    F: FnOnce(&crate::Store) -> T + Send + 'static,
    T: Send + 'static,
{
    let worker = state.clone();
    let metrics = media_server::query_metrics::QueryMetrics::current()
        .filter(media_server::query_metrics::QueryMetrics::diagnostic);
    let result = state
        .http_work
        .run_to_completion(move || {
            with_foreground_store(&worker, |store| observe_store_work(store, metrics, work))
        })
        .await;
    match result {
        Ok(value) => value,
        Err(crate::blocking_budget::BlockingError::Busy) => Err(busy_response()),
        Err(error) => Err(StoreCallError::Worker(error.to_string()).response()),
    }
}

/// Bounded foreground mutation for callers already on a blocking worker.
/// Check admission at the outer Store and again at every actual database writer.
pub(crate) fn with_foreground_store<T>(
    state: &ApiState,
    work: impl FnOnce(&crate::Store) -> T,
) -> Result<T, Response> {
    state.require_writable()?;
    state.require_write_admitted()?;
    with_write_admission(state, || work(&state.store))
}

/// Apply the same foreground policy at every actual database writer acquisition.
/// This scope is synchronous and runs only on a blocking worker.
pub(crate) fn with_write_admission<T>(
    state: &ApiState,
    work: impl FnOnce() -> T,
) -> Result<T, Response> {
    let writable = state.instance_writable.clone();
    let gate = state.write_gate.clone();
    let refusal = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let observed = refusal.clone();
    let result = state.writers.with_write_admission(
        STORE_WRITE_WAIT,
        move || {
            let response = if !writable.load(std::sync::atomic::Ordering::SeqCst) {
                Some(err(
                    StatusCode::CONFLICT,
                    "instance.read_only",
                    "当前实例为只读",
                ))
            } else {
                gate.admit().err().map(crate::write_gate::busy_response)
            };
            if let Some(response) = response {
                *observed.lock() = Some(response);
                return Err(store::StoreError::Protected(
                    "foreground write admission closed".into(),
                ));
            }
            Ok(())
        },
        work,
    );
    if let Some(response) = refusal.lock().take() {
        return Err(response);
    }
    result.map_err(|error| match error {
        store::StoreError::WriteBusy => busy_response(),
        error => {
            tracing::error!(%error, "database writer scope failed");
            err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "写入失败")
        }
    })
}

pub(super) fn observe_store_work<T>(
    store: &crate::Store,
    metrics: Option<media_server::query_metrics::QueryMetrics>,
    work: impl FnOnce(&crate::Store) -> T,
) -> T {
    let Some(metrics) = metrics else {
        return work(store);
    };
    let (value, stats) = store.with_stats(store::stats::StatsScope::Both, || work(store));
    metrics.update(|snapshot| {
        snapshot.statements = Some(snapshot.statements.unwrap_or(0) + stats.statements);
        snapshot.reader_checkouts =
            Some(snapshot.reader_checkouts.unwrap_or(0) + stats.reader_checkouts);
        snapshot.vm_steps = Some(snapshot.vm_steps.unwrap_or(0) + stats.vm_steps);
        snapshot.fullscan_steps = Some(snapshot.fullscan_steps.unwrap_or(0) + stats.fullscan_steps);
        snapshot.sort_operations =
            Some(snapshot.sort_operations.unwrap_or(0) + stats.sort_operations);
        snapshot.sqlite_profile_ms = Some(
            snapshot.sqlite_profile_ms.unwrap_or(0.) + stats.sql_duration_ns as f64 / 1_000_000.,
        );
    });
    value
}

/// [`store_read`] on a bare `Store` handle, for callers (and tests) that hold the
/// handle rather than the whole `ApiState`.
pub(crate) async fn store_read_handle<T, F>(
    store: std::sync::Arc<crate::Store>,
    deadline: Duration,
    work: F,
) -> Result<T, StoreCallError>
where
    F: FnOnce(&crate::Store) -> T + Send + 'static,
    T: Send + 'static,
{
    let wait = deadline.saturating_sub(LOCK_SLACK);
    let metrics = media_server::query_metrics::QueryMetrics::current()
        .filter(media_server::query_metrics::QueryMetrics::diagnostic);
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _cancel = CancelRead(cancelled.clone());
    let started = std::time::Instant::now();
    let absolute_deadline = started + deadline;
    let reader_deadline = started + wait;
    let task = tokio::task::spawn_blocking(move || {
        let budget = store::QueryBudget {
            deadline: absolute_deadline,
            cancelled,
        };
        store.with_query_budget(&budget, |store| {
            observe_store_work(store, metrics, |store| {
                store.with_read_snapshot(
                    reader_deadline.saturating_duration_since(std::time::Instant::now()),
                    || work(store),
                )
            })
            .map_err(store::LibraryReadError::Store)
        })
    });
    match tokio::time::timeout(deadline, task).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "Store reader could not finish within its budget");
            match error {
                store::LibraryReadError::Busy
                | store::LibraryReadError::Closed
                | store::LibraryReadError::Store(
                    store::StoreError::ReadBusy | store::StoreError::ReadClosed,
                )
                | store::LibraryReadError::Store(store::StoreError::WriteBusy) => {
                    Err(StoreCallError::Busy)
                }
                store::LibraryReadError::Cancelled
                | store::LibraryReadError::Deadline
                | store::LibraryReadError::Store(
                    store::StoreError::ReadDeadline | store::StoreError::ReadCancelled,
                ) => Err(StoreCallError::Deadline),
                error => Err(StoreCallError::Worker(error.to_string())),
            }
        }
        Ok(Err(error)) => Err(StoreCallError::Worker(error.to_string())),
        Err(_) => Err(StoreCallError::Deadline),
    }
}

struct CancelRead(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for CancelRead {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Background work owns only its actual per-database SQL leases.
pub(crate) async fn store_blocking<T, F>(
    store: std::sync::Arc<crate::Store>,
    work: F,
) -> Result<T, StoreCallError>
where
    F: FnOnce(&crate::Store) -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || work(&store))
        .await
        .map_err(|error| StoreCallError::Worker(error.to_string()))
}

#[cfg(test)]
mod tests;
