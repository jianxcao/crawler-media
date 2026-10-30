use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use rusqlite::{Connection, OpenFlags};

use super::{LibraryReadError, ReaderLease};
use crate::StoreError;
#[cfg(test)]
use crate::schema;
#[cfg(test)]
use crate::test_support::gate::Gate;

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn identity(path: &Path) -> Result<FileIdentity, StoreError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(super) struct State {
    pub(super) idle: Vec<Connection>,
    pub(super) total: usize,
    pub(super) closed: bool,
    pub(super) next_ticket: u64,
    pub(super) waiters: std::collections::VecDeque<(u64, Option<std::task::Waker>)>,
}

pub(super) struct Inner {
    path: PathBuf,
    identity: FileIdentity,
    /// Header values of the library file as of the last full layout validation. A
    /// lease compares them instead of re-walking ~127 schema objects twice per
    /// lease (D3).
    validated_stamp: Mutex<LayoutStamp>,
    cap: AtomicUsize,
    pub(super) database: &'static str,
    validate: fn(&Connection) -> Result<(), StoreError>,
    pub(super) planner_epoch: AtomicU64,
    pub(super) returned: std::sync::Condvar,
    pub(super) state: Mutex<State>,
    pub(super) stopped: AtomicBool,
    pub(super) snapshot_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pub(super) query_progress_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Test seam: when set, retiring a connection pauses until the test releases
    /// it. Bounded (`Gate` panics instead of hanging when the test never returns).
    #[cfg(test)]
    pub(super) retirement_gate: Mutex<Option<(Arc<Gate>, Arc<Gate>)>>,
    #[cfg(test)]
    pub(super) return_gate: Mutex<Option<(Arc<Gate>, Arc<Gate>)>>,
    /// Test seam: when set, the post-work layout re-check pauses here.
    #[cfg(test)]
    pub(super) postcheck_gate: Mutex<Option<(Arc<Gate>, Arc<Gate>)>>,
    #[cfg(test)]
    pub(super) postcheck_timeout_ms: Mutex<Option<Arc<std::sync::atomic::AtomicU64>>>,
}

pub struct ReaderPool {
    inner: Arc<Inner>,
}

impl ReaderPool {
    pub fn open(
        path: PathBuf,
        size: NonZeroUsize,
        database: &'static str,
        validate: fn(&Connection) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        let file_id = identity(&path)?;
        let conn = open_reader_for(&path, validate)?;
        if identity(&path)? != file_id {
            return Err(StoreError::Invalid(
                "library.db changed during reader open".into(),
            ));
        }
        let validated_stamp = layout_stamp(&conn)?;
        tracing::info!(path = %path.display(), capacity = size.get(), "library reader pool opened");
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                identity: file_id,
                validated_stamp: Mutex::new(validated_stamp),
                cap: AtomicUsize::new(size.get()),
                database,
                validate,
                planner_epoch: AtomicU64::new(0),
                returned: std::sync::Condvar::new(),
                state: Mutex::new(State {
                    idle: vec![conn],
                    total: 1,
                    closed: false,
                    next_ticket: 0,
                    waiters: Default::default(),
                }),
                stopped: AtomicBool::new(false),
                query_progress_observer: Mutex::new(None),
                snapshot_observer: Mutex::new(None),
                #[cfg(test)]
                retirement_gate: Mutex::new(None),
                #[cfg(test)]
                return_gate: Mutex::new(None),
                #[cfg(test)]
                postcheck_gate: Mutex::new(None),
                #[cfg(test)]
                postcheck_timeout_ms: Mutex::new(None),
            }),
        })
    }

    pub fn try_checkout(&self) -> Result<ReaderLease, LibraryReadError> {
        let ticket = super::wait::Ticket::new(Arc::clone(&self.inner))?;
        ticket.try_acquire()?.ok_or(LibraryReadError::Busy)
    }

    pub fn checkout(&self, wait: std::time::Duration) -> Result<ReaderLease, LibraryReadError> {
        super::wait::Ticket::new(Arc::clone(&self.inner))?.blocking(wait)
    }

    pub async fn checkout_async(
        &self,
        wait: std::time::Duration,
    ) -> Result<ReaderLease, LibraryReadError> {
        let ticket = super::wait::Ticket::new(Arc::clone(&self.inner))?;
        tokio::time::timeout(wait, std::future::poll_fn(|cx| ticket.poll(cx)))
            .await
            .map_err(|_| LibraryReadError::Busy)?
    }

    pub fn invalidate_planner(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        self.inner.planner_epoch.fetch_add(1, Ordering::AcqRel);
        let idle = std::mem::take(&mut state.idle);
        state.total -= idle.len();
        drop(state);
        drop(idle);
        self.inner.wake();
    }

    /// Configure startup fixtures on this same pool, never a parallel read pool.
    #[doc(hidden)]
    pub fn set_capacity(&self, size: NonZeroUsize) -> Result<(), StoreError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.total != state.idle.len() || !state.waiters.is_empty() {
            return Err(StoreError::Protected(
                "cannot resize an active reader pool".into(),
            ));
        }
        self.inner.cap.store(size.get(), Ordering::Release);
        let keep = size.get().min(state.idle.len());
        let removed = state.idle.split_off(keep);
        state.total -= removed.len();
        drop(state);
        drop(removed);
        Ok(())
    }

    /// Pending admissions, exposed for deterministic resource coordination fixtures.
    #[doc(hidden)]
    pub fn waiting(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .waiters
            .len()
    }

    pub fn available(&self) -> usize {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed { 0 } else { state.idle.len() }
    }

    pub(crate) fn owns(&self, lease: &super::ReaderLease) -> bool {
        lease.shares_pool(&self.inner)
    }

    pub fn close(&self) {
        self.inner.close();
    }

    pub fn check_open(&self) -> Result<(), LibraryReadError> {
        self.inner.check_identity()
    }

    /// Fixture boundary after a real Library read inside the active transaction.
    #[doc(hidden)]
    pub fn set_snapshot_observer(&self, observer: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .snapshot_observer
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = observer;
    }

    #[doc(hidden)]
    pub fn set_query_progress_observer(&self, observer: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .query_progress_observer
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = observer;
    }
}

impl Inner {
    pub(super) fn capacity(&self) -> usize {
        self.cap.load(Ordering::Acquire)
    }
    /// Cheap layout guard for one lease (D3): three header reads, whatever the size
    /// of the layout.
    ///
    /// Steady state compares the file's header stamp (application id, user version,
    /// schema cookie) with the stamp recorded by the last full validation. A
    /// mismatch means the file was rewritten under this pool, so the full validation
    /// runs once: if the layout is still ours the new stamp is recorded, and if it
    /// is not, the pool stops serving rather than read a schema it does not know.
    ///
    pub(super) fn check_layout(&self, conn: &Connection) -> Result<(), StoreError> {
        let stamp = layout_stamp(conn)?;
        let cached = *self
            .validated_stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if stamp == cached {
            return Ok(());
        }
        match (self.validate)(conn) {
            Ok(()) => {
                *self
                    .validated_stamp
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = stamp;
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    path = %self.path.display(),
                    error = %error,
                    "library reader layout rejected; closing pool"
                );
                self.close();
                Err(error)
            }
        }
    }

    pub(super) fn check_identity_raw(&self) -> Result<(), StoreError> {
        if identity(&self.path)? != self.identity {
            return Err(StoreError::Invalid(
                "library.db file identity changed".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn check_identity(&self) -> Result<(), LibraryReadError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(LibraryReadError::Closed);
        }
        if let Err(error) = self.check_identity_raw() {
            tracing::error!(path = %self.path.display(), error = %error, "library reader identity rejected");
            self.close();
            return Err(error.into());
        }
        Ok(())
    }

    pub(super) fn close(&self) {
        self.stopped.store(true, Ordering::Release);
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.closed {
            state.closed = true;
            state.total -= state.idle.len();
            let idle = std::mem::take(&mut state.idle);
            drop(state);
            drop(idle);
            tracing::info!(path = %self.path.display(), database = self.database, "reader pool closed");
            self.wake();
        }
    }

    pub(super) fn put_back(&self, conn: Connection, epoch: u64) {
        #[cfg(test)]
        {
            let gate = self.return_gate.lock().unwrap().clone();
            if let Some((entered, release)) = gate {
                entered.wait();
                release.wait();
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // Invalidation advances the epoch and drains idle connections under this
        // mutex. Check under that same lock so an old lease cannot reinsert its
        // connection after the invalidation has finished.
        if state.closed || epoch != self.planner_epoch.load(Ordering::Acquire) {
            drop(state);
            drop(conn); // Physical close precedes release of the capacity slot.
            self.retired();
        } else {
            state.idle.push(conn);
            drop(state);
            self.wake();
        }
    }

    pub(super) fn retired(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.total -= 1;
        drop(state);
        self.wake();
    }

    pub(super) fn wake(&self) {
        self.returned.notify_all();
        let wakers: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .waiters
            .iter_mut()
            .filter_map(|(_, w)| w.take())
            .collect();
        for waker in wakers {
            waker.wake();
        }
    }

    pub(super) fn open_connection(&self) -> Result<Connection, StoreError> {
        open_reader_for(&self.path, self.validate)
    }
}

fn open_reader_for(
    path: &Path,
    validate: fn(&Connection) -> Result<(), StoreError>,
) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.pragma_update(None, "query_only", "ON")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // 与写连接同样的页缓存 / mmap / temp_store；只读连接不设 journal_size_limit，
    // 也不跑写入型 PRAGMA optimize。
    crate::open::apply_reader_pragma_tuning(&conn)?;
    crate::open::register_casefold(&conn)?;
    crate::path_key::register(&conn)?;
    validate(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub(super) fn validate_reader(conn: &Connection) -> Result<(), StoreError> {
    schema::layout::validate_database_layout(
        "library",
        conn,
        schema::layout::LIBRARY_APPLICATION_ID,
        schema::layout::LIBRARY_USER_VERSION,
    )
}

/// The three header values that decide whether a cached layout validation is still
/// valid.
///
/// All three are needed: the schema cookie moves for pure DDL, while
/// `PRAGMA user_version` / `PRAGMA application_id` rewrite the header *without*
/// moving the cookie (that is the cutover signal). SQLite exposes one header value
/// per statement — the single-statement pragma table-valued form
/// (`SELECT … FROM pragma_user_version, …`) measured *worse*, 4 traced statements
/// per read, because SQLite traces each internal pragma execution.
#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutStamp {
    application_id: i64,
    user_version: i64,
    schema_version: i64,
}

fn layout_stamp(conn: &Connection) -> Result<LayoutStamp, StoreError> {
    let read = |name: &str| -> Result<i64, StoreError> {
        Ok(conn.pragma_query_value(None, name, |row| row.get(0))?)
    };
    Ok(LayoutStamp {
        application_id: read("application_id")?,
        user_version: read("user_version")?,
        schema_version: read("schema_version")?,
    })
}

#[cfg(test)]
mod pragma_tests {
    use super::{open_reader_for, validate_reader};

    /// 读池连接与写连接共用同一套 cache/mmap/temp_store（只读连接不设
    /// `journal_size_limit`，也不跑写入型 `PRAGMA optimize`）。
    #[test]
    fn reader_connections_share_writer_pragma_tuning() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path()).unwrap();
        let conn = open_reader_for(&dir.path().join("library.db"), validate_reader).unwrap();
        for (name, expected) in [
            ("cache_size", crate::open::CACHE_SIZE_KIB),
            ("temp_store", crate::open::TEMP_STORE_MEMORY),
            ("mmap_size", crate::open::MMAP_SIZE_BYTES),
        ] {
            let value: i64 = conn
                .pragma_query_value(None, name, |row| row.get(0))
                .unwrap();
            assert_eq!(value, expected, "读池连接 {name} 必须与写连接一致");
        }
        let query_only: i64 = conn
            .pragma_query_value(None, "query_only", |row| row.get(0))
            .unwrap();
        assert_eq!(query_only, 1, "读池连接必须只读");
        drop(store);
    }
}

#[cfg(test)]
mod epoch_tests;
