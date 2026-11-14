use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::{LibraryReadError, pool::Inner};
use crate::StoreError;

#[derive(Clone)]
pub struct QueryBudget {
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
}

impl QueryBudget {
    pub fn check(&self) -> Result<(), LibraryReadError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(LibraryReadError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(LibraryReadError::Deadline);
        }
        Ok(())
    }

    fn busy_wait(&self) -> Result<Duration, LibraryReadError> {
        self.check()?;
        Ok(self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(5000)))
    }
}

pub struct ReaderLease {
    conn: Option<Connection>,
    pool: Arc<Inner>,
    clean: bool,
    epoch: u64,
    session: Option<QueryBudget>,
    execution_policy: Option<QueryBudget>,
    #[cfg(test)]
    fail_cleanup: bool,
}

impl ReaderLease {
    pub(super) fn new(conn: Connection, pool: Arc<Inner>, epoch: u64) -> Self {
        Self {
            conn: Some(conn),
            pool,
            clean: true,
            epoch,
            session: None,
            execution_policy: None,
            #[cfg(test)]
            fail_cleanup: false,
        }
    }

    pub(super) fn shares_pool(&self, pool: &Arc<Inner>) -> bool {
        Arc::ptr_eq(&self.pool, pool)
    }

    #[cfg(test)]
    fn inject_cleanup_failure(&mut self) {
        self.fail_cleanup = true;
    }

    pub fn with_connection<T>(
        &mut self,
        budget: &QueryBudget,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, LibraryReadError> {
        self.with_connection_instrumented(budget, work, None, false)
    }

    /// Fixture boundary: one lease plus the SQL work it performed.
    ///
    /// The count is the lease's own statement traffic (validation, transaction
    /// bookkeeping and the caller's statements), so a test can pin it without
    /// reaching into the pool's connections.
    #[doc(hidden)]
    pub fn with_connection_counted<T>(
        &mut self,
        budget: &QueryBudget,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<(T, crate::library_query::items::stats::QueryWorkStats), LibraryReadError> {
        let mut stats = crate::library_query::items::stats::QueryWorkStats::default();
        let value = self.with_connection_instrumented(budget, work, Some(&mut stats), false)?;
        Ok((value, stats))
    }

    pub(super) fn with_connection_instrumented<T>(
        &mut self,
        budget: &QueryBudget,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
        stats: Option<&mut crate::library_query::items::stats::QueryWorkStats>,
        library_query: bool,
    ) -> Result<T, LibraryReadError> {
        if let Some(session) = &self.session {
            let effective = QueryBudget {
                deadline: budget.deadline.min(session.deadline),
                cancelled: if super::active_query_budget().is_some() {
                    budget.cancelled.clone()
                } else {
                    session.cancelled.clone()
                },
            };
            let conn = self.conn.as_ref().ok_or(LibraryReadError::Closed)?;
            let _scoped_capture = crate::stats::LeaseStats::for_database(self.pool.database, conn);
            let _capture = stats
                .map(|out| crate::library_query::items::stats::QueryWorkCapture::new(conn, out));
            effective.check()?;
            if self.pool.stopped.load(Ordering::Acquire) {
                return Err(LibraryReadError::Closed);
            }
            // SQLite's busy wait does not call the progress handler. Keep its
            // remaining wait bounded even when the execution policy is unchanged.
            conn.busy_timeout(effective.busy_wait()?)
                .map_err(StoreError::from)?;
            // Normal repeated reads retain the installed policy. A nested tighter
            // budget or different cancellation token must still interrupt active SQL.
            if self.execution_policy.as_ref().is_none_or(|installed| {
                installed.deadline != effective.deadline
                    || !Arc::ptr_eq(&installed.cancelled, &effective.cancelled)
            }) {
                install_progress(conn, &effective, &self.pool, true, false);
                self.execution_policy = Some(effective.clone());
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if library_query {
                    install_progress(conn, &effective, &self.pool, true, true);
                    observe_snapshot(conn, &self.pool)?;
                }
                work(conn)
            }));
            if library_query {
                install_progress(conn, &effective, &self.pool, true, false);
            }
            let result = match outcome {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            effective.check()?;
            return result.map_err(|error| {
                tracing::error!(database = self.pool.database, %error, "reader SQL failed");
                error.into()
            });
        }
        let conn = self.conn.as_ref().ok_or(LibraryReadError::Closed)?;
        let _scoped_capture = crate::stats::LeaseStats::for_database(self.pool.database, conn);
        let _capture =
            stats.map(|out| crate::library_query::items::stats::QueryWorkCapture::new(conn, out));
        Self::prepare(conn, &self.pool, &mut self.clean, budget)?;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            conn.execute_batch("BEGIN")?;
            install_progress(conn, budget, &self.pool, true, library_query);
            if library_query {
                observe_snapshot(conn, &self.pool)?;
            }
            work(conn)
        }));
        Self::finish_outcome(
            conn,
            &self.pool,
            &mut self.clean,
            budget,
            outcome,
            #[cfg(test)]
            &mut self.fail_cleanup,
        )
    }

    /// Establish one transaction and execution policy for a synchronous read stage.
    pub(crate) fn begin_session(
        &mut self,
        owner: &QueryBudget,
        budget: &QueryBudget,
    ) -> Result<(), LibraryReadError> {
        let conn = self.conn.as_ref().ok_or(LibraryReadError::Closed)?;
        let _capture = crate::stats::LeaseStats::for_database(self.pool.database, conn);
        Self::prepare(conn, &self.pool, &mut self.clean, budget)?;
        // Record the policy before BEGIN so even a failed BEGIN is cleaned on drop.
        self.session = Some(owner.clone());
        self.execution_policy = Some(budget.clone());
        let begin = conn.execute_batch("BEGIN").map_err(StoreError::from);
        budget.check()?;
        begin?;
        install_progress(conn, budget, &self.pool, true, false);
        Ok(())
    }

    pub(crate) fn finish_session(&mut self) -> Result<(), LibraryReadError> {
        let budget = self.session.take().ok_or(LibraryReadError::Closed)?;
        self.execution_policy = None;
        let conn = self.conn.as_ref().ok_or(LibraryReadError::Closed)?;
        let _capture = crate::stats::LeaseStats::for_database(self.pool.database, conn);
        Self::finish_outcome(
            conn,
            &self.pool,
            &mut self.clean,
            &budget,
            Ok(Ok(())),
            #[cfg(test)]
            &mut self.fail_cleanup,
        )
    }

    fn finish_outcome<T>(
        conn: &Connection,
        pool: &Arc<Inner>,
        clean: &mut bool,
        budget: &QueryBudget,
        outcome: std::thread::Result<Result<T, StoreError>>,
        #[cfg(test)] fail_cleanup: &mut bool,
    ) -> Result<T, LibraryReadError> {
        // The callback owns only this attempt's cancellation state. Remove it before
        // rollback so a late cancellation cannot interrupt cleanup or the next lease.
        conn.progress_handler(0, None::<fn() -> bool>);
        let cleanup_policy = conn.busy_timeout(remaining_wait(budget));
        #[allow(unused_mut)]
        let mut cleanup = if conn.is_autocommit() {
            Ok(())
        } else {
            conn.execute_batch("ROLLBACK")
        };
        #[cfg(test)]
        if std::mem::take(fail_cleanup) {
            cleanup = Err(rusqlite::Error::ExecuteReturnedResults);
        }
        #[cfg(test)]
        wait_gate(&pool.postcheck_gate);
        let mut post_policy = Ok(());
        let postcheck = if cleanup.is_ok() && matches!(outcome, Ok(Ok(_))) {
            post_policy = conn.busy_timeout(remaining_wait(budget));
            if post_policy.is_ok() {
                #[cfg(test)]
                if let Some(observed) = pool.postcheck_timeout_ms.lock().unwrap().as_ref() {
                    let configured: u64 = conn
                        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
                        .unwrap();
                    observed.store(configured, Ordering::Release);
                }
                install_progress(conn, budget, pool, false, false);
                let result = pool.check_layout(conn);
                conn.progress_handler(0, None::<fn() -> bool>);
                Some(result)
            } else {
                None
            }
        } else {
            None
        };
        let reset = conn.busy_timeout(Duration::from_millis(5000));
        if cleanup_policy.is_err() || cleanup.is_err() || post_policy.is_err() || reset.is_err() {
            *clean = false;
            tracing::error!(
                database = pool.database,
                ?cleanup_policy,
                ?cleanup,
                ?post_policy,
                ?reset,
                "reader cleanup failed; retiring connection"
            );
        }
        let identity = pool.check_identity();
        if identity.is_err() {
            *clean = false;
        }
        match outcome {
            Ok(result) => {
                if let Err(error) = cleanup_policy {
                    return Err(StoreError::from(error).into());
                }
                if let Err(error) = cleanup {
                    return Err(StoreError::from(error).into());
                }
                if let Err(error) = post_policy {
                    return Err(StoreError::from(error).into());
                }
                if let Err(error) = reset {
                    return Err(StoreError::from(error).into());
                }
                identity?;
                if let Some(Err(error)) = postcheck {
                    if error.is_layout_refusal() {
                        tracing::error!(database = pool.database, error = %error, "reader layout changed before publication");
                        pool.close();
                    }
                    budget.check()?;
                    return Err(error.into());
                }
                budget.check()?;
                if pool.stopped.load(Ordering::Acquire) {
                    return Err(LibraryReadError::Closed);
                }
                result.map_err(|error| {
                    tracing::error!(database = pool.database, error = %error, "reader SQL failed");
                    error.into()
                })
            }
            Err(panic) => {
                tracing::error!(
                    database = pool.database,
                    "reader work panicked after cleanup"
                );
                std::panic::resume_unwind(panic)
            }
        }
    }

    fn prepare(
        conn: &Connection,
        pool: &Arc<Inner>,
        clean: &mut bool,
        budget: &QueryBudget,
    ) -> Result<(), LibraryReadError> {
        if !*clean {
            return Err(LibraryReadError::Closed);
        }
        pool.check_identity()?;
        budget.check()?;
        conn.busy_timeout(budget.busy_wait()?)
            .map_err(StoreError::from)?;
        install_progress(conn, budget, pool, false, false);
        if let Err(error) = pool.check_layout(conn) {
            conn.progress_handler(0, None::<fn() -> bool>);
            let reset = conn.busy_timeout(Duration::from_millis(5000));
            if reset.is_err() {
                *clean = false;
            }
            if error.is_layout_refusal() {
                tracing::error!(database = pool.database, error = %error, "reader layout rejected at execution");
                pool.close();
            }
            budget.check()?;
            return Err(error.into());
        }

        Ok(())
    }
}

fn observe_snapshot(conn: &Connection, pool: &Inner) -> Result<(), StoreError> {
    let observer = pool
        .snapshot_observer
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(observer) = observer {
        // Deferred BEGIN alone does not pin WAL. Read before exposing the snapshot.
        conn.query_row("SELECT count(*) FROM libraries", [], |row| {
            row.get::<_, i64>(0)
        })?;
        observer();
    }
    Ok(())
}

fn remaining_wait(budget: &QueryBudget) -> Duration {
    budget
        .deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_millis(5000))
}

impl Drop for ReaderLease {
    fn drop(&mut self) {
        if self.session.is_some() {
            let _ = self.finish_session();
        }
        if let Some(conn) = self.conn.take() {
            if self.clean {
                self.pool.put_back(conn, self.epoch);
            } else {
                drop(conn); // Physical close precedes release of the capacity slot.
                #[cfg(test)]
                wait_gate(&self.pool.retirement_gate);
                self.pool.retired();
            }
        }
    }
}

/// Test seam only: pause the current thread inside one of the pool's gates.
///
/// The gates are `TimedGate` halves, so a test that forgets to release its side
/// fails with the gate's name instead of wedging the whole run.
#[cfg(test)]
fn wait_gate(
    gate: &std::sync::Mutex<
        Option<(
            Arc<crate::test_support::gate::Gate>,
            Arc<crate::test_support::gate::Gate>,
        )>,
    >,
) {
    let pair = gate.lock().unwrap().clone();
    if let Some((entered, release)) = pair {
        entered.wait();
        release.wait();
    }
}

fn install_progress(
    conn: &Connection,
    budget: &QueryBudget,
    pool: &Arc<Inner>,
    query_active: bool,
    library_query: bool,
) {
    let cancelled = Arc::clone(&budget.cancelled);
    let stopped = Arc::clone(pool);
    let deadline = budget.deadline;
    let observer = if query_active && (pool.database != "library" || library_query) {
        pool.query_progress_observer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    } else {
        None
    };
    let diagnostic = (query_active && pool.database == "library")
        .then(crate::progress::active)
        .flatten();
    let observed = AtomicBool::new(false);
    conn.progress_handler(
        if observer.is_some() {
            1
        } else {
            diagnostic.as_ref().map_or(1000, |p| p.interval)
        },
        Some(move || {
            if let Some(observer) = &observer
                && !observed.swap(true, Ordering::AcqRel)
            {
                observer();
            }
            diagnostic.as_ref().is_some_and(|p| p.run())
                || cancelled.load(Ordering::Acquire)
                || stopped.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
        }),
    );
}

#[cfg(test)]
mod tests;
