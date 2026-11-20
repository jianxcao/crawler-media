//! A request that cannot finish layout validation must not poison later reads.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use store::{Db, LibraryReadError, QueryBudget, ReaderPool, Store};

fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + Duration::from_secs(2),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

fn interrupted_validation_keeps_pool(db: Db, expire: bool) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let pool = store.reader_pool(db);
    check_validation_recovers(&store, &pool, db, expire);
}

fn check_validation_recovers(store: &Store, pool: &ReaderPool, db: Db, expire: bool) {
    let mut lease = pool.try_checkout().unwrap();
    let interrupted = Arc::new(AtomicBool::new(false));
    let armed = Arc::new(AtomicBool::new(false));
    let hook_armed = armed.clone();
    let request = QueryBudget {
        deadline: Instant::now() + Duration::from_millis(50),
        ..budget()
    };
    let cancelled = request.cancelled.clone();
    let observed = interrupted.clone();
    let deadline = request.deadline;
    lease
        .with_connection(&budget(), |conn| {
            conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                if hook_armed.load(Ordering::Acquire)
                    && matches!(
                        ctx.action,
                        AuthAction::Read {
                            table_name: "sqlite_master" | "sqlite_schema",
                            ..
                        }
                    )
                    && !observed.swap(true, Ordering::AcqRel)
                {
                    if expire {
                        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    } else {
                        cancelled.store(true, Ordering::Release);
                    }
                }
                Authorization::Allow
            }));
            Ok(())
        })
        .unwrap();
    // Diagnostic triggers are allowed by every read-pool layout validator and
    // reliably advance the schema cookie, forcing a full validation.
    let table = match db {
        Db::App => "users",
        Db::Subscribe => "subscribes",
        Db::Library => "libraries",
        Db::Cache => "tmdb_cache",
    };
    store
        .with_writer(db, |conn| {
            conn.execute_batch(&format!(
                "CREATE TRIGGER review_allowed_trigger AFTER INSERT ON {table} BEGIN SELECT 1; END;"
            ))?;
            Ok(())
        })
        .unwrap();
    armed.store(true, Ordering::Release);
    let result = lease.with_connection(&request, |conn| {
        Ok(conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
    });
    assert!(interrupted.load(Ordering::Acquire));
    if expire {
        assert!(
            matches!(result, Err(LibraryReadError::Deadline)),
            "{result:?}"
        );
    } else {
        assert!(
            matches!(result, Err(LibraryReadError::Cancelled)),
            "{result:?}"
        );
    }
    drop(lease);
    let mut next = pool
        .try_checkout()
        .expect("a later request keeps its reader pool");
    assert_eq!(
        next.with_connection(&budget(), |conn| {
            Ok(conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
        })
        .unwrap(),
        1
    );
}

#[test]
fn cancelled_layout_validation_does_not_close_app_readers() {
    interrupted_validation_keeps_pool(Db::App, false);
}

#[test]
fn cancelled_layout_validation_does_not_close_library_readers() {
    interrupted_validation_keeps_pool(Db::Library, false);
}

#[test]
fn cancelled_layout_validation_does_not_close_subscribe_readers() {
    interrupted_validation_keeps_pool(Db::Subscribe, false);
}

#[test]
fn cancelled_layout_validation_does_not_close_cache_readers() {
    interrupted_validation_keeps_pool(Db::Cache, false);
}

#[test]
fn expired_layout_validation_does_not_close_readers() {
    interrupted_validation_keeps_pool(Db::Library, true);
}

#[test]
fn cache_schema_validation_preserves_interrupt_errors() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let pool = ReaderPool::open(
        temp.path().join("cache.db"),
        std::num::NonZeroUsize::new(1).unwrap(),
        "cache",
        store::schema::layout::validate_cache_layout,
    )
    .unwrap();
    check_validation_recovers(&store, &pool, Db::Cache, false);
}
