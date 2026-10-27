use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use rusqlite::{Connection, OpenFlags};

use super::{LibraryReadError, LibraryReadLease};
#[cfg(test)]
use crate::test_support::gate::Gate;
use crate::{StoreError, schema};

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

struct State {
    idle: Vec<Connection>,
    total: usize,
    closed: bool,
}

pub(super) struct Inner {
    path: PathBuf,
    identity: FileIdentity,
    /// Header values of the library file as of the last full layout validation. A
    /// lease compares them instead of re-walking ~127 schema objects twice per
    /// lease (D3).
    validated_stamp: Mutex<LayoutStamp>,
    cap: usize,
    state: Mutex<State>,
    pub(super) stopped: AtomicBool,
    pub(super) snapshot_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pub(super) query_progress_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Test seam: when set, retiring a connection pauses until the test releases
    /// it. Bounded (`Gate` panics instead of hanging when the test never returns).
    #[cfg(test)]
    pub(super) retirement_gate: Mutex<Option<(Arc<Gate>, Arc<Gate>)>>,
    /// Test seam: when set, the post-work layout re-check pauses here.
    #[cfg(test)]
    pub(super) postcheck_gate: Mutex<Option<(Arc<Gate>, Arc<Gate>)>>,
    #[cfg(test)]
    pub(super) postcheck_timeout_ms: Mutex<Option<Arc<std::sync::atomic::AtomicU64>>>,
}

pub struct LibraryReadPool {
    inner: Arc<Inner>,
}

impl LibraryReadPool {
    pub(crate) fn open(path: PathBuf, size: NonZeroUsize) -> Result<Self, StoreError> {
        let file_id = identity(&path)?;
        let conn = open_reader(&path, true)?;
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
                cap: size.get(),
                state: Mutex::new(State {
                    idle: vec![conn],
                    total: 1,
                    closed: false,
                }),
                stopped: AtomicBool::new(false),
                query_progress_observer: Mutex::new(None),
                snapshot_observer: Mutex::new(None),
                #[cfg(test)]
                retirement_gate: Mutex::new(None),
                #[cfg(test)]
                postcheck_gate: Mutex::new(None),
                #[cfg(test)]
                postcheck_timeout_ms: Mutex::new(None),
            }),
        })
    }

    pub fn try_checkout(&self) -> Result<LibraryReadLease, LibraryReadError> {
        self.inner.check_identity()?;
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(LibraryReadError::Closed);
        }
        if let Some(conn) = state.idle.pop() {
            drop(state);
            return Ok(LibraryReadLease::new(conn, Arc::clone(&self.inner)));
        }
        if state.total == self.inner.cap {
            return Err(LibraryReadError::Busy);
        }
        state.total += 1; // A replacement reserves capacity before opening.
        drop(state);
        // A connection validates the layout once, when it joins the pool; leases
        // then trust the pool's recorded header stamp (see `Inner::check_layout`).
        let opened = open_reader(&self.inner.path, true);
        let identity_ok = self.inner.check_identity_raw().is_ok();
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed || !identity_ok {
            state.total -= 1;
            drop(state);
            self.close();
            return Err(LibraryReadError::Closed);
        }
        match opened {
            Ok(conn) => Ok(LibraryReadLease::new(conn, Arc::clone(&self.inner))),
            Err(error) => {
                state.total -= 1;
                tracing::error!(error = %error, "library reader open failed");
                Err(error.into())
            }
        }
    }

    pub fn available(&self) -> usize {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed { 0 } else { state.idle.len() }
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
        match validate_reader(conn) {
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

    fn check_identity_raw(&self) -> Result<(), StoreError> {
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
            tracing::info!(path = %self.path.display(), "library reader pool closed");
        }
    }

    pub(super) fn put_back(&self, conn: Connection) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            drop(state);
            drop(conn);
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.total -= 1;
        } else {
            state.idle.push(conn);
        }
    }

    pub(super) fn retired(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.total -= 1;
    }
}

fn open_reader(path: &Path, validate: bool) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.pragma_update(None, "query_only", "ON")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // 与写连接同样的页缓存 / mmap / temp_store；只读连接不设 journal_size_limit，
    // 也不跑写入型 PRAGMA optimize。
    crate::open::apply_reader_pragma_tuning(&conn)?;
    crate::open::register_casefold(&conn)?;
    if validate {
        validate_reader(&conn)?;
    }
    Ok(conn)
}

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
    use super::open_reader;

    /// 读池连接与写连接共用同一套 cache/mmap/temp_store（只读连接不设
    /// `journal_size_limit`，也不跑写入型 `PRAGMA optimize`）。
    #[test]
    fn reader_connections_share_writer_pragma_tuning() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path()).unwrap();
        let conn = open_reader(&dir.path().join("library.db"), true).unwrap();
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
