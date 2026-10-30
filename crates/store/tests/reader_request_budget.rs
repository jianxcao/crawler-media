//! Reusing a snapshot must still enforce the current read's execution budget.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use rusqlite::functions::FunctionFlags;
use store::{Db, LibraryReadError, QueryBudget, Store, StoreError};

fn fixture() -> (tempfile::TempDir, Store) {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn a_nested_shorter_deadline_interrupts_sql_on_an_already_bound_reader() {
    let (_dir, store) = fixture();
    store
        .with_read_snapshot(Duration::from_secs(2), || {
            store.list_libraries().unwrap();
            let started = Instant::now();
            let result = store.with_read_scope(Duration::from_millis(20), || {
                store.with_read(Db::Library, |conn| {
                    Ok(conn.query_row(
                        "WITH RECURSIVE n(x) AS (
                            SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 1000000000
                         ) SELECT sum(x) FROM n",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?)
                })
            });
            assert!(matches!(result, Err(StoreError::ReadDeadline)));
            assert!(
                started.elapsed() < Duration::from_millis(300),
                "the active SQL retained the outer deadline: {:?}",
                started.elapsed()
            );
            assert!(!store.list_libraries().unwrap().is_empty());
        })
        .unwrap();
    assert!(!store.list_libraries().unwrap().is_empty());
}

#[test]
fn a_new_cancel_token_interrupts_sql_on_an_already_bound_reader() {
    let (_dir, store) = fixture();
    let result = store.with_read_snapshot(Duration::from_secs(2), || {
        store.list_libraries().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let budget = QueryBudget {
            deadline: Instant::now() + Duration::from_secs(2),
            cancelled: cancelled.clone(),
        };
        let started = Instant::now();
        let result = store.with_query_budget(&budget, |store| {
            store
                .with_read(Db::Library, |conn| {
                    conn.create_scalar_function(
                        "cancel_bound_request",
                        0,
                        FunctionFlags::SQLITE_UTF8,
                        move |_| {
                            cancelled.store(true, Ordering::Release);
                            Ok(1_i64)
                        },
                    )?;
                    Ok(conn.query_row(
                        "WITH RECURSIVE n(x) AS (
                            SELECT cancel_bound_request()
                            UNION ALL SELECT x+1 FROM n WHERE x < 1000000000
                         ) SELECT sum(x) FROM n",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?)
                })
                .map_err(LibraryReadError::Store)
        });
        assert!(matches!(result, Err(LibraryReadError::Cancelled)));
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "the active SQL retained the outer cancellation token: {:?}",
            started.elapsed()
        );
        assert!(!store.list_libraries().unwrap().is_empty());
    });
    assert!(matches!(result, Err(StoreError::ReadCancelled)));
    assert!(!store.list_libraries().unwrap().is_empty());
}

#[test]
fn first_admission_in_a_shorter_read_scope_does_not_shorten_its_snapshot_owner() {
    let (_dir, store) = fixture();
    store
        .with_read_snapshot(Duration::from_secs(2), || {
            store
                .with_read_scope(Duration::from_millis(20), || store.list_libraries())
                .unwrap()
                .unwrap();
            std::thread::sleep(Duration::from_millis(30));
            assert!(!store.list_libraries().unwrap().is_empty());
        })
        .unwrap();
    assert!(!store.list_libraries().unwrap().is_empty());
}

#[test]
fn first_admission_in_a_cancel_scope_does_not_keep_its_token_after_return() {
    let (_dir, store) = fixture();
    let cancelled = Arc::new(AtomicBool::new(false));
    store
        .with_read_snapshot(Duration::from_secs(2), || {
            let inner = QueryBudget {
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: cancelled.clone(),
            };
            store
                .with_query_budget(&inner, |store| {
                    store.list_libraries().map_err(LibraryReadError::Store)
                })
                .unwrap();
            cancelled.store(true, Ordering::Release);
            assert!(!store.list_libraries().unwrap().is_empty());
        })
        .unwrap();
    assert!(!store.list_libraries().unwrap().is_empty());
}

#[test]
fn request_stats_include_the_whole_reader_session() {
    let (_dir, store) = fixture();
    let budget = QueryBudget {
        deadline: Instant::now() + Duration::from_secs(2),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let mut lease = store.reader_pool(Db::Library).try_checkout().unwrap();
    let (_, direct) = lease
        .with_connection_counted(&budget, |conn| {
            for _ in 0..2 {
                assert_eq!(
                    conn.query_row("SELECT 2", [], |row| row.get::<_, i64>(0))?,
                    2
                );
            }
            Ok(())
        })
        .unwrap();
    drop(lease);
    let (_, request) = store.with_stats(store::stats::StatsScope::Library, || {
        store
            .with_read_snapshot(Duration::from_secs(2), || {
                for _ in 0..2 {
                    assert_eq!(
                        store
                            .with_read(Db::Library, |conn| {
                                Ok(conn.query_row("SELECT 2", [], |row| row.get::<_, i64>(0))?)
                            })
                            .unwrap(),
                        2
                    );
                }
            })
            .unwrap();
    });
    assert_eq!(request.reader_checkouts, 1);
    assert_eq!(
        request.statements, direct.statements,
        "request measurements must include admission validation and transaction cleanup"
    );
}

#[test]
fn reused_reader_busy_wait_fits_the_remaining_request_budget() {
    let (_dir, store) = fixture();
    let wait = Duration::from_millis(200);
    let started = Instant::now();
    store
        .with_read_snapshot(wait, || {
            store.list_libraries().unwrap();
            std::thread::sleep(Duration::from_millis(60));
            store
                .with_read(Db::Library, |conn| {
                    let configured_ms: u64 =
                        conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
                    let remaining = wait.saturating_sub(started.elapsed());
                    assert!(
                        Duration::from_millis(configured_ms)
                            <= remaining + Duration::from_millis(5),
                        "busy wait {configured_ms}ms exceeds remaining budget {remaining:?}"
                    );
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
}
