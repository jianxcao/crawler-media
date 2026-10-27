use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use store::{LibraryReadError, QueryBudget, Store, StoreError};

fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + Duration::from_secs(10),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

/// A lease used to re-walk the whole layout (54 tables + 79 indexes, ~127 objects)
/// *twice* per lease: once in `prepare`, once in the post-work check. That is the
/// 277-statement-per-browse measurement the perf baseline pinned, of which the
/// business query is 6 statements. A steady-state lease must cost a constant
/// number of statements instead, independent of how big the layout grows.
#[test]
fn steady_state_lease_runs_constant_validation_statements() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let (value, stats) = lease
        .with_connection_counted(&budget(), |conn| {
            Ok(conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
        })
        .unwrap();
    assert_eq!(value, 1);
    println!("steady-state lease statements: {stats:?}");
    // Composition, in order: three header-stamp reads before the read
    // (application_id, user_version, schema_version), BEGIN, the caller's statement,
    // ROLLBACK, three header-stamp reads after it. Nine statements, and — unlike the
    // two full layout walks it replaces (261 statements: 2 × 127 objects + stamps) —
    // independent of how many tables and indexes the layout grows to.
    //
    // Three reads per side instead of one because SQLite exposes exactly one header
    // value per statement: the schema cookie moves for pure DDL, while
    // `PRAGMA user_version` / `PRAGMA application_id` rewrite the header without
    // moving the cookie (that is the cutover signal), so a cookie-only guard would
    // miss a layout swap entirely. The pragma table-valued form
    // (`SELECT … FROM pragma_user_version, …`) measured *worse*, 4 traced statements
    // per read, because SQLite traces each internal pragma execution.
    let bookkeeping = stats.statements - 1; // minus the business statement above
    assert!(
        bookkeeping <= 8,
        "a steady-state lease ran {bookkeeping} non-business statements ({} total): \
         it is re-validating the layout per lease",
        stats.statements
    );
}

/// The cheap steady-state check must still catch a real schema change: a writer
/// outside the pool adds an object the layout does not declare, so the next lease
/// has to refuse (and the pool has to stop serving) instead of trusting its cache.
#[test]
fn schema_change_forces_revalidation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let writer = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    writer.execute_batch("CREATE TABLE zz(x)").unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let error = lease
        .with_connection_counted(&budget(), |conn| {
            Ok(conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
        })
        .expect_err("a lease must not serve a file whose schema changed under it");
    assert!(
        matches!(
            error,
            LibraryReadError::Store(StoreError::LayoutMismatch(_))
        ),
        "expected a typed layout refusal, got {error:?}"
    );
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
}

#[test]
fn pool_exhaustion_is_busy_and_closed_pool_never_reopens_database() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(2).unwrap())
        .unwrap();
    let a = pool.try_checkout().unwrap();
    let b = pool.try_checkout().unwrap();
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Busy)));
    drop(a);
    assert_eq!(pool.available(), 1);
    pool.close();
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
    drop(b);
    assert_eq!(pool.available(), 0);
}

#[test]
fn custom_app_path_and_missing_library_never_create_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("custom").join("application.sqlite");
    let store = Store::open_at(dir.path(), &app).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    assert!(pool.try_checkout().is_ok());
    std::fs::remove_file(dir.path().join("library.db")).unwrap();
    assert!(pool.try_checkout().is_err());
    assert!(!dir.path().join("library.db").exists());
}

#[test]
fn normal_wal_writes_preserve_reader_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let writer = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    writer
        .execute("UPDATE libraries SET name = name || ' updated'", [])
        .unwrap();
    let lease = pool.try_checkout().unwrap();
    drop(lease);
    assert_eq!(pool.available(), 1);
}

#[test]
fn replacing_database_with_old_layout_closes_pool() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let path = dir.path().join("library.db");
    std::fs::rename(&path, dir.path().join("retired.db")).unwrap();
    let old = rusqlite::Connection::open(&path).unwrap();
    old.pragma_update(None, "application_id", 1129139249_i64)
        .unwrap();
    old.pragma_update(None, "user_version", 1_i64).unwrap();
    drop(old);
    assert!(pool.try_checkout().is_err());
    assert_eq!(pool.available(), 0);
}
