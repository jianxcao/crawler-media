//! Each database has one writer connection. Cross-database acquisition order is
//! app → subscribe → library → cache → jobs.
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::library_read::{active_query_budget, check_writer_budget};
use crate::{Store, StoreError, WriterRank, WriterRankGuard};

mod cleanup;
mod diagnostics;
use cleanup::WriterScope;
#[doc(hidden)]
pub use diagnostics::WriterSample;
pub(crate) use diagnostics::{OwnedWriteTimer, flush_pending};

/// Database selected by the shared writer handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
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
    app_seq: AtomicU64,
    pub(crate) rename_publications: Mutex<std::collections::HashMap<String, usize>>,
}

struct AppSeqGuard<'a>(&'a AtomicU64);

impl<'a> AppSeqGuard<'a> {
    fn enter(seq: &'a AtomicU64) -> Self {
        let prev = seq.fetch_add(1, Ordering::SeqCst);
        debug_assert!(
            prev.is_multiple_of(2),
            "app_seq must be even on Db::App entry"
        );
        Self(seq)
    }
}

impl Drop for AppSeqGuard<'_> {
    fn drop(&mut self) {
        let prev = self.0.fetch_add(1, Ordering::SeqCst);
        debug_assert!(
            !prev.is_multiple_of(2),
            "app_seq must be odd on Db::App exit"
        );
    }
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
            app_seq: AtomicU64::new(0),
            rename_publications: Mutex::default(),
        }
    }

    pub(crate) fn app_seq(&self) -> u64 {
        self.app_seq.load(Ordering::SeqCst)
    }

    pub(crate) fn wait_for_even_app_seq(
        &self,
        deadline: std::time::Instant,
    ) -> Result<u64, StoreError> {
        let mut backoff = Duration::from_micros(50);
        loop {
            let seq = self.app_seq();
            if seq.is_multiple_of(2) {
                return Ok(seq);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(StoreError::WriteBusy);
            }
            std::thread::sleep(backoff.min(deadline.saturating_duration_since(now)));
            backoff = (backoff * 2).min(Duration::from_millis(2));
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

    /// Share an HTTP mutation scope's total deadline with external writers.
    pub fn remaining_write_wait(wait: Duration) -> Duration {
        ADMISSION.with(|scope| {
            scope.borrow().as_ref().map_or(wait, |s| {
                s.deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(wait)
            })
        })
    }

    pub fn check_write_admission() -> Result<(), StoreError> {
        ADMISSION.with(|scope| {
            let scope = scope.borrow();
            if let Some(admission) = scope.as_ref()
                && let Err(error) = (admission.check)()
            {
                *admission.failure.borrow_mut() = Some(AdmissionFailure::Refused);
                return Err(error);
            }
            Ok(())
        })
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
        self.with_writer_inner(wait, db, true, work)
    }

    /// Borrow a database's single writer strictly for storage/schema maintenance
    /// (`PRAGMA optimize`, `incremental_vacuum`, `VACUUM`) that never mutates logical
    /// or actor/auth rows (`users`, `settings`, `device_credentials`, `library_members`).
    pub fn with_maintenance_writer<T>(
        &self,
        db: Db,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_writer_inner(Duration::from_secs(1), db, false, work)
    }

    fn with_writer_inner<T>(
        &self,
        wait: Duration,
        db: Db,
        track_app_seq: bool,
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
        let mut measured = diagnostics::Measurement::new(db);
        let cleanup_errors = Rc::new(RefCell::new(Vec::new()));
        // Unwind the SQL scope, capture, MutexGuard and rank before telemetry.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _rank = WriterRankGuard::enter(db.rank())?;
            let mutex = match db {
                Db::App => &self.app,
                Db::Subscribe => &self.subscribe,
                Db::Library => &self.library,
                Db::Cache => &self.cache,
            };
            // Declared before the guard so its timestamp follows the actual unlock.
            let _hold_end = measured.hold_end();
            let Some(conn) = mutex.try_lock_for(wait) else {
                if let Some(budget) = &budget {
                    check_writer_budget(budget)?;
                }
                if let Some((_, _, failure)) = &admission {
                    *failure.borrow_mut() = Some(AdmissionFailure::Busy);
                }
                return Err(StoreError::WriteBusy);
            };
            measured.acquired();
            if let Some(budget) = &budget {
                check_writer_budget(budget)?;
            }
            if let Some((_, check, failure)) = admission
                && let Err(error) = check()
            {
                *failure.borrow_mut() = Some(AdmissionFailure::Refused);
                return Err(error);
            }
            let _app_seq =
                (track_app_seq && db == Db::App).then(|| AppSeqGuard::enter(&self.app_seq));
            let mut scope = WriterScope::new(&conn, budget.as_ref(), cleanup_errors.clone())?;
            let _capture = crate::stats::LeaseStats::new(db, &conn);
            let result = work(&conn);
            let cleanup = scope.finish();
            match result {
                Ok(value) => cleanup.map(|()| value),
                Err(error) => {
                    if let Err(cleanup_error) = cleanup {
                        cleanup_errors.borrow_mut().push(cleanup_error);
                    }
                    Err(error)
                }
            }
        }));
        let outcome = match &result {
            Ok(Ok(_)) => "ok",
            Ok(Err(StoreError::WriteBusy)) => "busy",
            Ok(Err(_)) => "error",
            Err(_) => "panic",
        };
        measured.released(outcome);
        measured.emit(std::mem::take(&mut *cleanup_errors.borrow_mut()));
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    pub(crate) fn has_rename_publication(&self, library_id: &str) -> bool {
        self.rename_publications
            .lock()
            .get(library_id)
            .copied()
            .unwrap_or(0)
            > 0
    }

    pub(crate) fn has_any_rename_publication(&self) -> bool {
        self.rename_publications
            .lock()
            .values()
            .any(|count| *count > 0)
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
