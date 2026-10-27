use std::cell::Cell;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::{LibraryReadError, QueryBudget};
use crate::{Store, StoreError};

thread_local! { static SHORT_STAGE_ACTIVE: Cell<bool> = const { Cell::new(false) }; }

struct ShortPolicy<'a> {
    app: &'a Connection,
    library: &'a Connection,
}

impl ShortPolicy<'_> {
    fn install(conn: &Connection, budget: &QueryBudget) -> Result<(), LibraryReadError> {
        budget.check()?;
        let remaining = budget
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(5000));
        conn.busy_timeout(remaining).map_err(StoreError::from)?;
        let cancelled: Arc<AtomicBool> = Arc::clone(&budget.cancelled);
        let deadline = budget.deadline;
        conn.progress_handler(
            1000,
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

impl Drop for ShortPolicy<'_> {
    fn drop(&mut self) {
        Self::reset(self.app);
        Self::reset(self.library);
        SHORT_STAGE_ACTIVE.with(|active| active.set(false));
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
        let entered = SHORT_STAGE_ACTIVE.with(|active| {
            if active.get() {
                false
            } else {
                active.set(true);
                true
            }
        });
        if !entered {
            return Err(LibraryReadError::NestedBudget);
        }
        let guard = ShortPolicy {
            app: &self.app,
            library: &self.library,
        };
        ShortPolicy::install(guard.app, budget)?;
        if let Err(error) = ShortPolicy::install(guard.library, budget) {
            drop(guard);
            return Err(error);
        }
        let result = work(self);
        drop(guard);
        budget.check()?;
        result
    }
}
