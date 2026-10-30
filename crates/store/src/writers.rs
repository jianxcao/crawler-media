//! Each database has one writer connection. Cross-database acquisition order is
//! app → subscribe → library → cache → jobs.
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::library_read::{active_query_budget, check_writer_budget};
use crate::{Store, StoreError, WriterRank, WriterRankGuard};

mod cleanup;
use cleanup::WriterScope;

/// Database selected by the shared writer handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Db {
    App,
    Subscribe,
    Library,
    Cache,
}

/// App operations on an already acquired connection. No operation acquires it again.
pub struct App<'a> {
    pub(crate) app: &'a Connection,
}

impl<'a> App<'a> {
    /// Borrow App operations on a connection already owned by a reader lease.
    pub fn from_connection(app: &'a Connection) -> Self {
        Self { app }
    }
}

pub struct Writers {
    app: Mutex<Connection>,
    subscribe: Mutex<Connection>,
    library: Mutex<Connection>,
    cache: Mutex<Connection>,
}

struct Admission {
    deadline: std::time::Instant,
    check: Rc<dyn Fn() -> Result<(), StoreError>>,
    failure: Rc<RefCell<Option<AdmissionFailure>>>,
}
#[derive(Clone, Copy)]
enum AdmissionFailure {
    Busy,
    Refused,
}

thread_local! {
    static ADMISSION: RefCell<Option<Admission>> = const { RefCell::new(None) };
}

impl Writers {
    pub(crate) fn new(
        app: Connection,
        subscribe: Connection,
        library: Connection,
        cache: Connection,
    ) -> Self {
        Self {
            app: Mutex::new(app),
            subscribe: Mutex::new(subscribe),
            library: Mutex::new(library),
            cache: Mutex::new(cache),
        }
    }

    /// Scope request admission to each actual mutation, after its writer is acquired.
    /// The callback is synchronous and the policy is removed on panic as well.
    pub fn with_write_admission<T>(
        &self,
        wait: Duration,
        admit: impl Fn() -> Result<(), StoreError> + 'static,
        work: impl FnOnce() -> T,
    ) -> Result<T, StoreError> {
        struct Restore(Option<Admission>);
        impl Drop for Restore {
            fn drop(&mut self) {
                ADMISSION.with(|scope| *scope.borrow_mut() = self.0.take());
            }
        }
        let failure = Rc::new(RefCell::new(None));
        let previous = ADMISSION.with(|scope| {
            scope.replace(Some(Admission {
                deadline: std::time::Instant::now() + wait,
                check: Rc::new(admit),
                failure: Rc::clone(&failure),
            }))
        });
        let _restore = Restore(previous);
        let value = work();
        let failed = *failure.borrow();
        match failed {
            Some(AdmissionFailure::Busy) => Err(StoreError::WriteBusy),
            Some(AdmissionFailure::Refused) => {
                Err(StoreError::Protected("write admission refused".into()))
            }
            None => Ok(value),
        }
    }

    pub fn with_writer<T>(
        &self,
        db: Db,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer_timeout(Duration::from_secs(1), db, work)
    }

    pub fn with_writer_timeout<T>(
        &self,
        wait: Duration,
        db: Db,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let admission = ADMISSION.with(|scope| {
            scope
                .borrow()
                .as_ref()
                .map(|s| (s.deadline, Rc::clone(&s.check), Rc::clone(&s.failure)))
        });
        let mut wait = admission.as_ref().map_or(wait, |(deadline, _, _)| {
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(wait)
        });
        let budget = active_query_budget();
        if let Some(budget) = &budget {
            check_writer_budget(budget)?;
            wait = wait.min(
                budget
                    .deadline
                    .saturating_duration_since(std::time::Instant::now()),
            );
        }
        let _rank = WriterRankGuard::enter(db.rank());
        let mutex = match db {
            Db::App => &self.app,
            Db::Subscribe => &self.subscribe,
            Db::Library => &self.library,
            Db::Cache => &self.cache,
        };
        let conn = mutex.try_lock_for(wait);
        let Some(conn) = conn else {
            if let Some(budget) = &budget {
                check_writer_budget(budget)?;
            }
            if let Some((_, _, failure)) = &admission {
                *failure.borrow_mut() = Some(AdmissionFailure::Busy);
            }
            return Err(StoreError::WriteBusy);
        };
        if let Some(budget) = &budget {
            check_writer_budget(budget)?;
        }
        if let Some((_, check, failure)) = admission
            && let Err(error) = check()
        {
            *failure.borrow_mut() = Some(AdmissionFailure::Refused);
            return Err(error);
        }
        let mut scope = WriterScope::new(&conn, budget.as_ref())?;
        let _capture = crate::stats::LeaseStats::new(db, &conn);
        let result = work(&conn);
        let cleanup = scope.finish();
        match result {
            Ok(value) => cleanup.map(|()| value),
            Err(error) => {
                if let Err(cleanup_error) = cleanup {
                    tracing::error!(%cleanup_error, "failed to clean up rejected Store writer transaction");
                }
                Err(error)
            }
        }
    }

    pub fn with_app<T>(
        &self,
        work: impl FnOnce(&App<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer(Db::App, |app| work(&App { app }))
    }
}

impl Store {
    pub fn writers(&self) -> Arc<Writers> {
        Arc::clone(&self.writers)
    }

    pub(crate) fn with_app<T>(
        &self,
        work: impl FnOnce(&App<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.writers.with_app(work)
    }

    pub(crate) fn with_app_read<T>(
        &self,
        work: impl FnOnce(&App<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_read(Db::App, |app| work(&App { app }))
    }
}

impl Drop for Writers {
    fn drop(&mut self) {
        for (database, conn) in [
            ("app", self.app.get_mut()),
            ("subscribe", self.subscribe.get_mut()),
            ("library", self.library.get_mut()),
            ("cache", self.cache.get_mut()),
        ] {
            if let Err(error) = conn.execute_batch("PRAGMA optimize;") {
                tracing::warn!(database, %error, "关闭连接前的 PRAGMA optimize 失败");
            }
        }
    }
}

impl Db {
    fn rank(self) -> WriterRank {
        match self {
            Self::App => WriterRank::App,
            Self::Subscribe => WriterRank::Subscribe,
            Self::Library => WriterRank::Library,
            Self::Cache => WriterRank::Cache,
        }
    }
}
