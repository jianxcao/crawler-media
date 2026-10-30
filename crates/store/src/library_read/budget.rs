use std::cell::RefCell;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::{LibraryReadError, QueryBudget};
use crate::{Store, StoreError};

thread_local! {
    static ACTIVE_BUDGET: RefCell<Option<QueryBudget>> = const { RefCell::new(None) };
}

pub(crate) fn active_query_budget() -> Option<QueryBudget> {
    ACTIVE_BUDGET.with(|active| active.borrow().clone())
}

/// A writer receives the operation's budget only after acquiring its mutex.
/// The connection must still be held when this policy is dropped.
pub(crate) struct WriterQueryPolicy<'a>(&'a Connection);

impl<'a> WriterQueryPolicy<'a> {
    pub(crate) fn install(conn: &'a Connection, budget: &QueryBudget) -> Result<Self, StoreError> {
        ShortPolicy::install(conn, budget, 1).map_err(writer_budget_error)?;
        Ok(Self(conn))
    }
}

impl Drop for WriterQueryPolicy<'_> {
    fn drop(&mut self) {
        ShortPolicy::reset(self.0);
    }
}

pub(crate) fn check_writer_budget(budget: &QueryBudget) -> Result<(), StoreError> {
    budget.check().map_err(writer_budget_error)
}

fn writer_budget_error(error: LibraryReadError) -> StoreError {
    match error {
        LibraryReadError::Store(error) => error,
        // with_query_budget maps the operation's cancellation/deadline after
        // its callback returns. At the Connection seam this is a SQLite interrupt.
        error => rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
            Some(error.to_string()),
        )
        .into(),
    }
}

struct ShortPolicy;

impl ShortPolicy {
    fn install(
        conn: &Connection,
        budget: &QueryBudget,
        interval: i32,
    ) -> Result<(), LibraryReadError> {
        budget.check()?;
        let remaining = budget
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(5000));
        conn.busy_timeout(remaining).map_err(StoreError::from)?;
        let cancelled: Arc<AtomicBool> = Arc::clone(&budget.cancelled);
        let deadline = budget.deadline;
        conn.progress_handler(
            interval,
            Some(move || cancelled.load(Ordering::Acquire) || Instant::now() >= deadline),
        );
        Ok(())
    }
    fn reset(conn: &Connection) {
        conn.progress_handler(0, None::<fn() -> bool>);
        if let Err(error) = conn.busy_timeout(Duration::from_millis(5000)) {
            tracing::error!(%error, "failed to restore Store query busy policy");
        }
    }
}

struct BudgetScope;
impl Drop for BudgetScope {
    fn drop(&mut self) {
        ACTIVE_BUDGET.with(|active| *active.borrow_mut() = None);
    }
}

impl Store {
    /// Runs a synchronous, owned-result Store phase. Caller owns the Store lock and the
    /// callback must not await or acquire that lock again. This does not acquire a writer lease.
    pub fn with_query_budget<T>(
        &self,
        budget: &QueryBudget,
        work: impl FnOnce(&Store) -> Result<T, LibraryReadError>,
    ) -> Result<T, LibraryReadError> {
        let entered = ACTIVE_BUDGET.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some() {
                false
            } else {
                *active = Some(budget.clone());
                true
            }
        });
        if !entered {
            return Err(LibraryReadError::NestedBudget);
        }
        let guard = BudgetScope;
        budget.check()?;
        let result = work(self);
        drop(guard);
        budget.check()?;
        result
    }
}
