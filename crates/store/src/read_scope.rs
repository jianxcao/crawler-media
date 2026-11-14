//! Read stages declare their databases and admit snapshots in one shared order.
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
    InvalidScope(&'static str),
}
impl Failure {
    fn error(self) -> StoreError {
        match self {
            Self::Busy => StoreError::ReadBusy,
            Self::Closed => StoreError::ReadClosed,
            Self::Deadline => StoreError::ReadDeadline,
            Self::Cancelled => StoreError::ReadCancelled,
            Self::InvalidScope(message) => StoreError::Protected(message.into()),
        }
    }
}
struct Scope {
    deadline: Instant,
    failure: Rc<RefCell<Option<Failure>>>,
}
thread_local! { static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) }; }
struct BoundReader {
    db: Db,
    pool: Arc<ReaderPool>,
    lease: Rc<RefCell<ReaderLease>>,
}
thread_local! {
    static READERS: RefCell<Vec<BoundReader>> = const { RefCell::new(Vec::new()) };
    static SNAPSHOTS: RefCell<Vec<Snapshot>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_READS: RefCell<Vec<Db>> = const { RefCell::new(Vec::new()) };
}
struct Snapshot {
    budget: QueryBudget,
    pools: Vec<Arc<ReaderPool>>,
}

fn rank(db: Db) -> usize {
    match db {
        Db::Library => 0,
        Db::App => 1,
        Db::Subscribe => 2,
        Db::Cache => 3,
    }
}

fn reject_scope(message: &'static str) -> StoreError {
    SCOPE.with(|scope| {
        if let Some(scope) = scope.borrow().as_ref() {
            *scope.failure.borrow_mut() = Some(Failure::InvalidScope(message));
        }
    });
    StoreError::Protected(message.into())
}

fn check_declared(pool: &Arc<ReaderPool>) -> Result<(), StoreError> {
    let declared = SNAPSHOTS.with(|snapshots| {
        snapshots.borrow().iter().all(|snapshot| {
            snapshot
                .pools
                .iter()
                .any(|declared| Arc::ptr_eq(pool, declared))
        })
    });
    if declared {
        Ok(())
    } else {
        Err(reject_scope(
            "read database was not declared by the snapshot stage",
        ))
    }
}

fn check_order(db: Db) -> Result<(), StoreError> {
    let reversed = READERS.with(|readers| {
        readers
            .borrow()
            .iter()
            .any(|reader| rank(reader.db) >= rank(db))
    }) || ACTIVE_READS
        .with(|readers| readers.borrow().iter().any(|held| rank(*held) >= rank(db)));
    if reversed {
        Err(reject_scope(
            "reader acquisition violates the database order",
        ))
    } else {
        Ok(())
    }
}

struct ActiveRead;
impl ActiveRead {
    fn enter(db: Db) -> Self {
        ACTIVE_READS.with(|readers| readers.borrow_mut().push(db));
        Self
    }
}
impl Drop for ActiveRead {
    fn drop(&mut self) {
        ACTIVE_READS.with(|readers| {
            readers.borrow_mut().pop();
        });
    }
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
            budget.deadline = budget.deadline.min(snapshot.budget.deadline);
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
        check_declared(&pool)?;
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
        check_order(db)?;
        let budget = query_budget();
        lease.begin_session(&budget, &budget).map_err(read_error)?;
        READERS.with(|readers| {
            readers.borrow_mut().push(BoundReader {
                db,
                pool,
                lease: Rc::new(RefCell::new(lease)),
            });
        });
        let mut restore = ReaderScope(depth);
        let value = work(self);
        restore.finish()?;
        Ok(value)
    }

    /// Admit exactly the declared databases before running a pure-read stage.
    /// Sorted admission prevents opposite SQL orders from forming a pool wait cycle.
    /// Nested stages may narrow the declaration, but cannot expand the outer set.
    pub fn with_read_snapshot<T>(
        &self,
        databases: &[Db],
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
            owner_budget.check().map_err(read_error)?;
            let mut databases = databases.to_vec();
            databases.sort_unstable_by_key(|db| rank(*db));
            databases.dedup();
            let pools: Vec<_> = databases.iter().map(|db| self.reader_pool(*db)).collect();
            for pool in &pools {
                check_declared(pool)?;
            }
            let mut readers = ReaderScope(READERS.with(|r| r.borrow().len()));
            for (db, pool) in databases.into_iter().zip(&pools) {
                let bound = READERS.with(|readers| {
                    readers
                        .borrow()
                        .iter()
                        .any(|reader| Arc::ptr_eq(&reader.pool, pool))
                });
                if bound {
                    continue;
                }
                check_order(db)?;
                let mut lease = pool
                    .checkout(
                        owner_budget
                            .deadline
                            .saturating_duration_since(Instant::now()),
                    )
                    .map_err(read_error)?;
                lease
                    .begin_session(&owner_budget, &owner_budget)
                    .map_err(read_error)?;
                READERS.with(|readers| {
                    readers.borrow_mut().push(BoundReader {
                        db,
                        pool: pool.clone(),
                        lease: Rc::new(RefCell::new(lease)),
                    })
                });
            }
            SNAPSHOTS.with(|snapshots| {
                snapshots.borrow_mut().push(Snapshot {
                    budget: owner_budget,
                    pools,
                })
            });
            let _restore = Restore;
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
            check_declared(&pool).map_err(LibraryReadError::Store)?;
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
            check_order(db).map_err(LibraryReadError::Store)?;
            let mut lease =
                pool.checkout(budget.deadline.saturating_duration_since(Instant::now()))?;
            budget.check()?;
            let _active = ActiveRead::enter(db);
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
