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
    static SNAPSHOTS: RefCell<Vec<QueryBudget>> = const { RefCell::new(Vec::new()) };
}
struct ReaderScope(usize);
impl ReaderScope {
    fn finish(&mut self) -> Result<(), StoreError> {
        // Remove slots before cleanup: neither SQL nor pool IO runs under TLS borrows.
        let readers = READERS.with(|readers| readers.borrow_mut().split_off(self.0));
        let mut error = None;
        for reader in readers.into_iter().rev() {
            if let Err(failed) = reader.lease.borrow_mut().finish_session() {
                error.get_or_insert_with(|| read_error(failed));
            }
        }
        error.map_or(Ok(()), Err)
    }
}
impl Drop for ReaderScope {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            tracing::error!(%error, "reader scope cleanup failed");
        }
    }
}

fn read_error(error: LibraryReadError) -> StoreError {
    match error {
        LibraryReadError::Busy => StoreError::ReadBusy,
        LibraryReadError::Closed => StoreError::ReadClosed,
        LibraryReadError::Deadline => StoreError::ReadDeadline,
        LibraryReadError::Cancelled => StoreError::ReadCancelled,
        LibraryReadError::Store(error) => error,
        error => StoreError::Invalid(error.to_string()),
    }
}

fn query_budget() -> QueryBudget {
    let mut budget = crate::library_read::active_query_budget().unwrap_or_else(|| QueryBudget {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    if let Some(deadline) = SCOPE.with(|s| s.borrow().as_ref().map(|s| s.deadline)) {
        budget.deadline = budget.deadline.min(deadline);
    }
    SNAPSHOTS.with(|snapshots| {
        for snapshot in snapshots.borrow().iter() {
            budget.deadline = budget.deadline.min(snapshot.deadline);
        }
    });
    budget
}

impl Store {
    /// Bind an already admitted reader to this synchronous Store stage.
    /// Other databases use their own pool; reentering an active connection fails.
    pub fn with_reader_lease<T>(
        &self,
        db: Db,
        mut lease: ReaderLease,
        work: impl FnOnce(&Store) -> T,
    ) -> Result<T, StoreError> {
        let pool = self.reader_pool(db);
        if !pool.owns(&lease) {
            return Err(StoreError::ReadLeaseMismatch);
        }
        let depth = READERS.with(|readers| {
            let readers = readers.borrow();
            if readers
                .iter()
                .any(|reader| Arc::ptr_eq(&reader.pool, &pool))
            {
                return Err(StoreError::ReadReentrant);
            }
            Ok(readers.len())
        })?;
        let budget = query_budget();
        lease.begin_session(&budget, &budget).map_err(read_error)?;
        READERS.with(|readers| {
            readers.borrow_mut().push(BoundReader {
                pool,
                lease: Rc::new(RefCell::new(lease)),
            });
        });
        let mut restore = ReaderScope(depth);
        let value = work(self);
        restore.finish()?;
        Ok(value)
    }

    /// One synchronous read stage, with one lease and WAL snapshot per used database.
    /// Connections are admitted on first use and released before returning owned data.
    pub fn with_read_snapshot<T>(
        &self,
        wait: Duration,
        work: impl FnOnce() -> T,
    ) -> Result<T, StoreError> {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                SNAPSHOTS.with(|snapshots| {
                    snapshots.borrow_mut().pop();
                });
            }
        }
        self.with_read_scope(wait, || {
            let owner_budget = query_budget();
            SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().push(owner_budget));
            let _restore = Restore;
            let mut readers = ReaderScope(READERS.with(|r| r.borrow().len()));
            let value = work();
            readers.finish()?;
            Ok(value)
        })?
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
        let budget = query_budget();
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
            let owner = SNAPSHOTS.with(|snapshots| snapshots.borrow().last().cloned());
            if let Some(owner) = owner {
                // The snapshot belongs to the enclosing pure-read stage. Its first
                // SQL may have a shorter nested deadline or temporary cancel token.
                lease.begin_session(&owner, &budget)?;
                let bound = Rc::new(RefCell::new(lease));
                READERS.with(|readers| {
                    readers.borrow_mut().push(BoundReader {
                        pool,
                        lease: Rc::clone(&bound),
                    })
                });
                return bound.borrow_mut().with_connection(&budget, work);
            }
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
