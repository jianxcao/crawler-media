//! Request policy is thread local; connections are borrowed only at the SQL seam.
use crate::{Db, LibraryReadError, QueryBudget, ReaderLease, ReaderPool, Store, StoreError};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Failure {
    Busy,
    Closed,
    Deadline,
    Cancelled,
}
impl Failure {
    fn error(self) -> StoreError {
        match self {
            Self::Busy => StoreError::ReadBusy,
            Self::Closed => StoreError::ReadClosed,
            Self::Deadline => StoreError::ReadDeadline,
            Self::Cancelled => StoreError::ReadCancelled,
        }
    }
}
struct Scope {
    deadline: Instant,
    failure: Rc<RefCell<Option<Failure>>>,
}
thread_local! { static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) }; }
struct BoundReader {
    pool: Arc<ReaderPool>,
    lease: Rc<RefCell<ReaderLease>>,
}
thread_local! {
    static READERS: RefCell<Vec<BoundReader>> = const { RefCell::new(Vec::new()) };
}
struct ReaderScope(usize);
impl Drop for ReaderScope {
    fn drop(&mut self) {
        READERS.with(|readers| readers.borrow_mut().truncate(self.0));
    }
}

impl Store {
    /// Bind an already admitted reader to this synchronous Store stage.
    /// Other databases use their own pool; reentering an active connection fails.
    pub fn with_reader_lease<T>(
        &self,
        db: Db,
        lease: ReaderLease,
        work: impl FnOnce(&Store) -> T,
    ) -> Result<T, StoreError> {
        let pool = self.reader_pool(db);
        if !pool.owns(&lease) {
            return Err(StoreError::ReadLeaseMismatch);
        }
        let depth = READERS.with(|readers| {
            let mut readers = readers.borrow_mut();
            if readers
                .iter()
                .any(|reader| Arc::ptr_eq(&reader.pool, &pool))
            {
                return Err(StoreError::ReadReentrant);
            }
            let depth = readers.len();
            readers.push(BoundReader {
                pool,
                lease: Rc::new(RefCell::new(lease)),
            });
            Ok(depth)
        })?;
        let _restore = ReaderScope(depth);
        Ok(work(self))
    }

    /// Preserve reader admission failures even if the inner HTTP closure maps them.
    pub fn with_read_scope<T>(
        &self,
        wait: Duration,
        work: impl FnOnce() -> T,
    ) -> Result<T, StoreError> {
        struct Restore(Option<Scope>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SCOPE.with(|s| *s.borrow_mut() = self.0.take());
            }
        }
        let failure = Rc::new(RefCell::new(None));
        let previous = SCOPE.with(|s| {
            s.replace(Some(Scope {
                deadline: Instant::now() + wait,
                failure: Rc::clone(&failure),
            }))
        });
        let _restore = Restore(previous);
        let value = work();
        let failed = *failure.borrow();
        failed.map_or(Ok(value), |f| Err(f.error()))
    }

    pub fn with_read<T>(
        &self,
        db: Db,
        work: impl FnOnce(&rusqlite::Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let scope = SCOPE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|s| (s.deadline, Rc::clone(&s.failure)))
        });
        let mut budget =
            crate::library_read::active_query_budget().unwrap_or_else(|| QueryBudget {
                deadline: Instant::now() + Duration::from_secs(5),
                cancelled: Arc::new(AtomicBool::new(false)),
            });
        if let Some((deadline, _)) = &scope {
            budget.deadline = budget.deadline.min(*deadline);
        }
        let result = (|| {
            budget.check()?;
            let pool = self.reader_pool(db);
            // Clone the slot, then release the thread-local container before SQL:
            // a callback may read another database without borrowing READERS again.
            let bound = READERS.with(|readers| {
                readers
                    .borrow()
                    .iter()
                    .find(|reader| Arc::ptr_eq(&reader.pool, &pool))
                    .map(|reader| Rc::clone(&reader.lease))
            });
            if let Some(bound) = bound {
                let mut lease = bound
                    .try_borrow_mut()
                    .map_err(|_| LibraryReadError::Store(StoreError::ReadReentrant))?;
                return lease.with_connection(&budget, work);
            }
            let mut lease =
                pool.checkout(budget.deadline.saturating_duration_since(Instant::now()))?;
            budget.check()?;
            lease.with_connection(&budget, work)
        })();
        result.map_err(|error| {
            let failure = match error {
                LibraryReadError::Busy => Some(Failure::Busy),
                LibraryReadError::Closed => Some(Failure::Closed),
                LibraryReadError::Deadline => Some(Failure::Deadline),
                LibraryReadError::Cancelled => Some(Failure::Cancelled),
                _ => None,
            };
            if let Some(failure) = failure {
                if let Some((_, slot)) = scope {
                    *slot.borrow_mut() = Some(failure);
                }
                return failure.error();
            }
            match error {
                LibraryReadError::Store(error) => error,
                error => StoreError::Invalid(error.to_string()),
            }
        })
    }
}
