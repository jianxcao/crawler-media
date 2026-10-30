//! Bounded Store stages for request-owned Library reads.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use crate::{Store, management::ApiState};
use std::future::Future;
use store::{LibraryReadError, QueryBudget};
use store::{LibraryReadRequest, LibraryReadResult, ReadCredential};

mod metrics;
mod mutation;
use media_server::query_metrics::QueryMetrics;
use metrics::{StageMetrics, WorkerMetrics};
pub(crate) use mutation::run_store_write_stage_named;

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) async fn run_store_query_stage<T: Send + 'static>(
    state: &ApiState,
    deadline: Instant,
    work: impl FnOnce(&Store) -> Result<T, LibraryReadError> + Send + 'static,
) -> Result<T, LibraryReadError> {
    run_store_query_stage_named(state, deadline, "prepare", work).await
}

pub(crate) async fn run_store_query_stage_named<T: Send + 'static>(
    state: &ApiState,
    deadline: Instant,
    stage: &'static str,
    work: impl FnOnce(&Store) -> Result<T, LibraryReadError> + Send + 'static,
) -> Result<T, LibraryReadError> {
    let phase = StageMetrics::start(stage);
    if Instant::now() >= deadline {
        phase.timeout();
        return Err(LibraryReadError::Deadline);
    }
    let store = state.store();
    let wait_started = Instant::now();
    let admitted = store
        .reader_pool(store::Db::Library)
        .checkout_async(deadline.saturating_duration_since(Instant::now()))
        .await;
    let reader_wait = wait_started.elapsed();
    state.record_query_phase("lock_wait", reader_wait);
    if let Some(metrics) = QueryMetrics::current() {
        let wait_phase = match stage {
            "validate" => "validate_wait",
            "credential_binding" => "binding_wait",
            _ => "prepare_wait",
        };
        metrics.duration(wait_phase, reader_wait);
    }
    let lease = admitted.inspect_err(|error| {
        if matches!(error, LibraryReadError::Busy) {
            phase.rejected();
        }
        if Instant::now() >= deadline {
            phase.timeout();
        }
    })?;
    let reservation = state.http_work.try_reserve().map_err(|_| {
        phase.rejected();
        LibraryReadError::Busy
    })?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelOnDrop(cancelled.clone());
    let stage_barrier = state.library_read_stage_barrier.clone();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let request_metrics = QueryMetrics::current();
    let worker = WorkerMetrics::queued(request_metrics, stage, deadline);
    let result = reservation
        .run(remaining, move || {
            let mut worker = worker.start();
            let budget = QueryBudget {
                deadline,
                cancelled,
            };
            budget.check()?;
            if let Some(barrier) = stage_barrier {
                barrier(stage);
            }
            worker.acquired();
            budget.check()?;
            store.with_query_budget(&budget, |store| {
                store
                    .with_reader_lease(store::Db::Library, lease, |store| {
                        store
                            .with_read_scope(
                                deadline.saturating_duration_since(Instant::now()),
                                || work(store),
                            )
                            .map_err(LibraryReadError::Store)?
                    })
                    .map_err(LibraryReadError::Store)?
            })
        })
        .await;
    drop(cancel_on_drop);
    match result {
        Ok(value) => {
            if matches!(&value, Err(LibraryReadError::Deadline)) {
                phase.timeout();
            }
            value
        }
        Err(
            crate::blocking_budget::BlockingError::Busy
            | crate::blocking_budget::BlockingError::Timeout,
        ) => {
            phase.timeout();
            Err(LibraryReadError::Deadline)
        }
        Err(crate::blocking_budget::BlockingError::Worker(error)) => {
            Err(LibraryReadError::Store(store::StoreError::Protected(error)))
        }
    }
}

/// Identity reads use the shared App pool independently of external IO capacity.
pub(crate) async fn run_app_read_stage_named<T: Send + 'static>(
    state: &ApiState,
    deadline: Instant,
    stage: &'static str,
    work: impl FnOnce(&store::App<'_>) -> Result<T, store::StoreError> + Send + 'static,
) -> Result<T, LibraryReadError> {
    let phase = StageMetrics::start(stage);
    if Instant::now() >= deadline {
        phase.timeout();
        return Err(LibraryReadError::Deadline);
    }
    // Wait asynchronously before occupying a blocking worker. The unified App
    // pool itself bounds identity work; external IO must not deny authentication.
    let mut lease = state
        .store()
        .reader_pool(store::Db::App)
        .checkout_async(deadline.saturating_duration_since(Instant::now()))
        .await
        .inspect_err(|_| phase.timeout())?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelOnDrop(cancelled.clone());
    let stage_barrier = state.library_read_stage_barrier.clone();
    let worker = WorkerMetrics::queued(QueryMetrics::current(), stage, deadline);
    let task = tokio::task::spawn_blocking(move || {
        let mut worker = worker.start();
        let budget = QueryBudget {
            deadline,
            cancelled,
        };
        budget.check()?;
        if let Some(barrier) = stage_barrier {
            barrier(stage);
        }
        worker.acquired();
        lease.with_connection(&budget, |connection| {
            work(&store::App::from_connection(connection))
        })
    });
    let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), task).await;
    drop(cancel_on_drop);
    match result {
        Ok(Ok(value)) => {
            if value.as_ref().is_err_and(error_is_unavailable) {
                phase.timeout();
            }
            value
        }
        Ok(Err(error)) => Err(LibraryReadError::Store(store::StoreError::Protected(
            error.to_string(),
        ))),
        Err(_) => {
            phase.timeout();
            Err(LibraryReadError::Deadline)
        }
    }
}

pub(crate) async fn run_lease_stage(
    state: &ApiState,
    context: store::LibraryReadContext,
    deadline: Instant,
) -> Result<(store::LibraryReadContext, LibraryReadResult), LibraryReadError> {
    let phase = StageMetrics::start("lease");
    if Instant::now() >= deadline {
        phase.timeout();
        return Err(LibraryReadError::Deadline);
    }
    let mut lease = state
        .library_reads
        .checkout_async(deadline.saturating_duration_since(Instant::now()))
        .await
        .inspect_err(|_| phase.timeout())?;
    let reservation = state.http_work.try_reserve().map_err(|_| {
        phase.rejected();
        LibraryReadError::Busy
    })?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelOnDrop(cancelled.clone());
    let metrics = state.clone();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let request_metrics = QueryMetrics::current();
    let worker = WorkerMetrics::queued(request_metrics.clone(), "lease", deadline);
    let result = reservation
        .run(remaining, move || {
            let worker = worker.start();
            let budget = QueryBudget {
                deadline,
                cancelled,
            };
            budget.check()?;
            let selection_started = Instant::now();
            let result = if request_metrics
                .as_ref()
                .is_some_and(QueryMetrics::diagnostic)
            {
                let (result, stats) = lease.execute_observed(&context, &budget);
                if let Some(m) = &request_metrics {
                    m.update(|s| {
                        s.vm_steps = Some(s.vm_steps.unwrap_or(0) + stats.vm_steps);
                        s.sort_operations =
                            Some(s.sort_operations.unwrap_or(0) + stats.sort_operations);
                        s.fullscan_steps =
                            Some(s.fullscan_steps.unwrap_or(0) + stats.fullscan_steps);
                        s.statements = Some(s.statements.unwrap_or(0) + stats.statements);
                        s.sqlite_profile_ms = Some(
                            s.sqlite_profile_ms.unwrap_or(0.)
                                + stats.sql_duration_ns as f64 / 1_000_000.,
                        );
                    });
                }
                result
            } else {
                lease.execute(&context, &budget)
            };
            worker.duration("sql", selection_started);
            metrics.record_query_phase("store_select", selection_started.elapsed());
            drop(lease);
            result.map(|result| (context, result))
        })
        .await;
    drop(cancel_on_drop);
    match result {
        Ok(value) => {
            if matches!(&value, Err(LibraryReadError::Deadline)) {
                phase.timeout();
            }
            value
        }
        Err(
            crate::blocking_budget::BlockingError::Busy
            | crate::blocking_budget::BlockingError::Timeout,
        ) => {
            phase.timeout();
            Err(LibraryReadError::Deadline)
        }
        Err(crate::blocking_budget::BlockingError::Worker(error)) => {
            Err(LibraryReadError::Store(store::StoreError::Protected(error)))
        }
    }
}

/// Preserve Self's initial missing/hidden Library 404 without conflating it
/// with invalid credentials. The leased attempt still owns final authorization.
pub(crate) async fn run_self_library_read(
    state: &ApiState,
    credential: ReadCredential,
    library: domain::LibraryId,
    request: LibraryReadRequest,
    deadline: Instant,
) -> Result<LibraryReadResult, axum::response::Response> {
    let owned_credential = credential.clone();
    let owned_request = request.clone();
    let visible = run_store_query_stage_named(state, deadline, "visibility", move |store| {
        let context = store.prepare_library_read(&owned_credential, &owned_request)?;
        Ok(context.has_library(library))
    })
    .await
    .map_err(stage_response)?;
    if !visible {
        tracing::warn!(library_id = %library, "Requested Library is missing or hidden");
        return Err(crate::http::library::missing_library());
    }
    run_library_read(state, credential, request, deadline)
        .await
        .map_err(stage_response)
}

pub(crate) async fn run_library_read(
    state: &ApiState,
    credential: ReadCredential,
    request: LibraryReadRequest,
    deadline: Instant,
) -> Result<LibraryReadResult, LibraryReadError> {
    run_library_read_with(
        state,
        credential,
        request,
        deadline,
        |result| async { Ok(result) },
        std::convert::identity,
    )
    .await
}

pub(crate) async fn run_library_read_with<T, E, F, Fut, M>(
    state: &ApiState,
    credential: ReadCredential,
    request: LibraryReadRequest,
    deadline: Instant,
    finish: F,
    map_error: M,
) -> Result<T, E>
where
    F: Fn(LibraryReadResult) -> Fut,
    Fut: Future<Output = Result<T, E>>,
    M: Fn(LibraryReadError) -> E,
{
    for attempt in 0..2 {
        let owned_credential = credential.clone();
        let owned_request = request.clone();
        let context = run_store_query_stage_named(state, deadline, "prepare", move |store| {
            store.prepare_library_read(&owned_credential, &owned_request)
        })
        .await
        .map_err(&map_error)?;
        let (context, result) = match run_lease_stage(state, context, deadline).await {
            Ok(result) => result,
            Err(LibraryReadError::ContextChanged) if attempt == 0 => {
                QueryMetrics::retry();
                continue;
            }
            Err(error) => return Err(map_error(error)),
        };
        let (returned, total) = metrics::page_counts(&result);
        let finished = finish(result).await?;
        state.library_reads.check_open().map_err(&map_error)?;
        // Keep context opaque and owned until all cold work has ended.
        // The lease worker above must return its connection before this Store stage.
        let validation = run_store_query_stage_named(state, deadline, "validate", move |store| {
            store.validate_library_read(&context)
        })
        .await;
        match validation {
            Ok(()) => {
                state.library_reads.check_open().map_err(&map_error)?;
                if Instant::now() >= deadline {
                    return Err(map_error(LibraryReadError::Deadline));
                }
                QueryMetrics::page(returned, total);
                return Ok(finished);
            }
            Err(LibraryReadError::ContextChanged) if attempt == 0 => {
                QueryMetrics::retry();
                continue;
            }
            Err(error) => return Err(map_error(error)),
        }
    }
    Err(map_error(LibraryReadError::ContextChanged))
}

/// Blocking cold work keeps the same request budget without holding Store.
pub(crate) async fn run_cold_query_stage<T: Send + 'static>(
    state: &ApiState,
    deadline: Instant,
    work: impl FnOnce(QueryBudget) -> Result<T, media_server::provider::QueryError> + Send + 'static,
) -> Result<T, media_server::provider::QueryError> {
    use media_server::provider::QueryError;
    let phase = StageMetrics::start("cold");
    if Instant::now() >= deadline {
        phase.timeout();
        return Err(QueryError::Unavailable);
    }
    let reservation = state.http_work.try_reserve().map_err(|_| {
        phase.rejected();
        QueryError::Unavailable
    })?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelOnDrop(cancelled.clone());
    let remaining = deadline.saturating_duration_since(Instant::now());
    let request_metrics = QueryMetrics::current();
    let worker = WorkerMetrics::queued(request_metrics, "cold", deadline);
    let result = reservation
        .run(remaining, move || {
            let _worker = worker.start();
            let budget = QueryBudget {
                deadline,
                cancelled,
            };
            budget.check().map_err(|_| QueryError::Unavailable)?;
            let outcome = work(QueryBudget {
                deadline,
                cancelled: budget.cancelled.clone(),
            });
            budget.check().map_err(|_| QueryError::Unavailable)?;
            outcome
        })
        .await;
    drop(cancel_on_drop);
    match result {
        Ok(value) => {
            if Instant::now() >= deadline {
                phase.timeout();
            }
            value
        }
        Err(
            crate::blocking_budget::BlockingError::Busy
            | crate::blocking_budget::BlockingError::Timeout,
        ) => {
            phase.timeout();
            Err(QueryError::Unavailable)
        }
        Err(crate::blocking_budget::BlockingError::Worker(error)) => {
            Err(QueryError::Internal(error))
        }
    }
}

pub(crate) fn error_is_unavailable(error: &LibraryReadError) -> bool {
    matches!(
        error,
        LibraryReadError::Busy
            | LibraryReadError::Closed
            | LibraryReadError::Deadline
            | LibraryReadError::Cancelled
            | LibraryReadError::ContextChanged
    ) || matches!(error, LibraryReadError::Store(source) if store_error_is_unavailable(source))
}

pub(crate) fn stage_response(error: LibraryReadError) -> axum::response::Response {
    use axum::http::StatusCode;
    // A rejected request (undecodable cursor, out-of-range filter value) is the
    // caller's error, not a failed read: answer with the same 400 shape
    // `browse_store_error` gives the very same `StoreError`. Reporting it as a 500
    // would make bad input indistinguishable from a real read failure.
    if let LibraryReadError::Store(store::StoreError::Invalid(message)) = &error {
        tracing::warn!(%error, "Library read rejected a malformed request");
        return crate::http::err(StatusCode::BAD_REQUEST, "library.invalid", message);
    }
    let code = if matches!(
        &error,
        LibraryReadError::Store(store::StoreError::QueryUnavailable { .. })
    ) {
        "library.reindexing"
    } else {
        "library.read"
    };
    let status = if error_is_unavailable(&error) {
        StatusCode::SERVICE_UNAVAILABLE
    } else if matches!(error, LibraryReadError::Unauthorized) {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    tracing::error!(%error, "Bounded Library Store stage failed");
    crate::http::err(status, code, "Library 读取失败")
}

/// SQLite can report contention before a submillisecond absolute budget expires.
pub(crate) fn store_error_is_unavailable(error: &store::StoreError) -> bool {
    match error {
        store::StoreError::QueryUnavailable { .. }
        | store::StoreError::ReadBusy
        | store::StoreError::ReadClosed
        | store::StoreError::ReadDeadline
        | store::StoreError::ReadCancelled
        | store::StoreError::WriteBusy => true,
        store::StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ),
        _ => false,
    }
}

#[cfg(test)]
mod contention_tests {
    use super::*;

    #[test]
    fn actual_sqlite_writer_contention_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.db");
        let first = rusqlite::Connection::open(&path).unwrap();
        let second = rusqlite::Connection::open(&path).unwrap();
        first
            .execute_batch(
                "CREATE TABLE t (id INTEGER); BEGIN IMMEDIATE; INSERT INTO t VALUES (1);",
            )
            .unwrap();
        second.busy_timeout(std::time::Duration::ZERO).unwrap();
        let error = second.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        let error = store::StoreError::Sqlite(error);
        assert!(store_error_is_unavailable(&error));
        assert_eq!(
            stage_response(LibraryReadError::Store(error)).status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let error = second.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert_eq!(
            crate::http::library::browse_store_error(store::StoreError::Sqlite(error)).status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
