use std::num::NonZeroUsize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::test_support::gate::{DEFAULT_GATE_TIMEOUT, Gate, TimedGate};
use crate::{LibraryReadError, Store};

use super::QueryBudget;

fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + Duration::from_secs(3),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

/// Wait for a value a worker thread must produce. Bounded so a worker that dies
/// before sending fails this test instead of hanging it.
fn expect_from_worker<T>(rx: &std::sync::mpsc::Receiver<T>, what: &str) -> T {
    rx.recv_timeout(DEFAULT_GATE_TIMEOUT)
        .unwrap_or_else(|_| panic!("worker never reported {what}"))
}

#[test]
fn reader_refuses_main_temp_and_schema_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    for sql in [
        "UPDATE libraries SET name = name",
        "CREATE TEMP TABLE accidental_write (id INTEGER)",
        "CREATE TABLE accidental_schema (id INTEGER)",
    ] {
        assert!(
            lease
                .with_connection(&budget(), |conn| {
                    conn.execute_batch(sql)?;
                    Ok(())
                })
                .is_err(),
            "write unexpectedly accepted: {sql}"
        );
    }
    let count = lease
        .with_connection(&budget(), |conn| {
            Ok(conn.query_row("SELECT count(*) FROM libraries", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .unwrap();
    assert!(count >= 0);
}

#[test]
fn expired_lease_does_not_interrupt_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let first = budget();
    let late_cancel = Arc::clone(&first.cancelled);
    let mut lease = pool.try_checkout().unwrap();
    let (barrier, worker_barrier) = TimedGate::new("first request running");
    let worker = std::thread::spawn(move || {
        let result = lease.with_connection(&first, |conn| {
            worker_barrier.wait();
            let _: i64 = conn.query_row(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 10000000) SELECT sum(x) FROM n",
                [], |row| row.get(0),
            )?;
            Ok(())
        });
        assert!(matches!(result, Err(LibraryReadError::Cancelled)));
        drop(lease);
    });
    barrier.wait();
    late_cancel.store(true, Ordering::Release);
    worker.join().unwrap();
    late_cancel.store(false, Ordering::Release);
    let mut next = pool.try_checkout().unwrap();
    let (entered, worker_entered) = TimedGate::new("next request entered");
    let (release, worker_release) = TimedGate::new("next request released");
    let next_worker = std::thread::spawn(move || {
        next.with_connection(&budget(), |conn| {
            worker_entered.wait();
            worker_release.wait();
            Ok(conn.query_row(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 100000) SELECT sum(x) FROM n",
                [], |row| row.get::<_, i64>(0),
            )?)
        })
    });
    entered.wait(); // The second attempt installed its progress callback.
    late_cancel.store(true, Ordering::Release);
    release.wait();
    assert!(next_worker.join().unwrap().unwrap() > 100000);
}

fn paused_sql(
    conn: &rusqlite::Connection,
    entered: Arc<Gate>,
    release: Arc<Gate>,
) -> Result<(), crate::StoreError> {
    conn.create_scalar_function(
        "gate",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |_| {
            entered.wait();
            release.wait();
            Ok(1_i64)
        },
    )?;
    let _: i64 = conn.query_row(
        "WITH RECURSIVE n(x) AS (SELECT gate() UNION ALL SELECT x+1 FROM n WHERE x < 10000000) SELECT sum(x) FROM n",
        [], |row| row.get(0),
    )?;
    Ok(())
}

#[test]
fn active_sql_deadline_and_close_cancel_then_drain() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let (entered, worker_entered) = TimedGate::new("deadline sql entered");
    let (release, worker_release) = TimedGate::new("deadline sql released");
    let mut lease = pool.try_checkout().unwrap();
    let (deadline_tx, deadline_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        // The window starts where the work starts: on a slow box thread start-up
        // must not eat the budget this test is measuring.
        let budget = QueryBudget {
            deadline: Instant::now() + Duration::from_millis(100),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        deadline_tx.send(budget.deadline).unwrap();
        lease.with_connection(&budget, |conn| {
            paused_sql(conn, worker_entered, worker_release)
        })
    });
    let deadline = expect_from_worker(&deadline_rx, "its deadline");
    entered.wait(); // SQL is executing in gate().
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
    release.wait();
    assert!(matches!(
        worker.join().unwrap(),
        Err(LibraryReadError::Deadline)
    ));

    let (entered, worker_entered) = TimedGate::new("closed sql entered");
    let (release, worker_release) = TimedGate::new("closed sql released");
    let mut lease = pool.try_checkout().unwrap();
    let worker = std::thread::spawn(move || {
        let result = lease.with_connection(&budget(), |conn| {
            paused_sql(conn, worker_entered, worker_release)
        });
        drop(lease);
        result
    });
    entered.wait();
    pool.close();
    assert_eq!(pool.available(), 0);
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
    release.wait();
    assert!(matches!(
        worker.join().unwrap(),
        Err(LibraryReadError::Closed)
    ));
    assert_eq!(pool.available(), 0);
}

#[test]
fn validation_and_dirty_retirement_keep_capacity_reserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    lease.inject_cleanup_failure();
    let _ = lease.with_connection(&budget(), |conn| {
        Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)
    });
    // The pool holds one end of each gate, the test the other: the lease's drop
    // pauses in `wait_gate` until this test lets it go.
    let (lease_entered, entered) = TimedGate::new("retirement entered");
    let (lease_release, release) = TimedGate::new("retirement released");
    *lease.pool.retirement_gate.lock().unwrap() = Some((lease_entered, lease_release));
    let worker = std::thread::spawn(move || drop(lease));
    entered.wait();
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Busy)));
    release.wait();
    worker.join().unwrap();
    assert_eq!(pool.available(), 0);
}

#[test]
fn checkout_busy_is_immediate_while_real_layout_validation_is_paused() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = Arc::new(
        store
            .open_library_read_pool(NonZeroUsize::new(1).unwrap())
            .unwrap(),
    );
    let lease = pool.try_checkout().unwrap();
    let (hook_entered, entered) = TimedGate::new("validation paused");
    let (hook_release, release) = TimedGate::new("validation released");
    let once = Arc::new(AtomicBool::new(false));
    let hook_once = Arc::clone(&once);
    lease
        .conn
        .as_ref()
        .unwrap()
        .authorizer(Some(move |ctx: AuthContext<'_>| {
            if matches!(
                ctx.action,
                AuthAction::Pragma {
                    pragma_name: "application_id",
                    ..
                }
            ) && !hook_once.swap(true, Ordering::AcqRel)
            {
                hook_entered.wait();
                hook_release.wait();
            }
            Authorization::Allow
        }));
    drop(lease);
    let worker_pool = Arc::clone(&pool);
    let worker = std::thread::spawn(move || {
        let mut lease = worker_pool.try_checkout().unwrap();
        lease.with_connection(&budget(), |conn| {
            Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)
        })
    });
    entered.wait();
    let (sent, received) = mpsc::channel();
    let contender_pool = Arc::clone(&pool);
    let contender = std::thread::spawn(move || {
        sent.send(matches!(
            contender_pool.try_checkout(),
            Err(LibraryReadError::Busy)
        ))
        .unwrap()
    });
    let immediate = received.recv_timeout(Duration::from_millis(200));
    release.wait();
    contender.join().unwrap();
    assert!(
        immediate.unwrap(),
        "checkout waited behind SQLite validation"
    );
    assert_eq!(worker.join().unwrap().unwrap(), 1);
}

#[test]
fn panic_and_sql_error_leave_no_transaction_or_callback() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    assert!(
        lease
            .with_connection(&budget(), |conn| {
                conn.execute_batch("SELECT * FROM absent_table")?;
                Ok(())
            })
            .is_err()
    );
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), _> = lease.with_connection(&budget(), |_conn| panic!("fault"));
    }));
    drop(lease);
    assert_eq!(pool.available(), 1);
    let mut next = pool.try_checkout().unwrap();
    let state = next
        .with_connection(&budget(), |conn| {
            let query_only: i64 = conn.pragma_query_value(None, "query_only", |r| r.get(0))?;
            Ok((conn.is_autocommit(), query_only))
        })
        .unwrap();
    assert_eq!(state, (false, 1)); // with_connection owns the current read transaction.
}

#[test]
fn same_inode_layout_change_after_checkout_is_rejected() {
    for change in [
        "PRAGMA user_version=1",
        "PRAGMA application_id=1",
        "DROP INDEX library_default",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let pool = store
            .open_library_read_pool(NonZeroUsize::new(1).unwrap())
            .unwrap();
        let mut lease = pool.try_checkout().unwrap();
        let writer = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
        writer.execute_batch(change).unwrap();
        assert!(
            lease
                .with_connection(&budget(), |conn| {
                    Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)
                })
                .is_err(),
            "accepted {change}"
        );
        assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
    }
}

#[test]
fn layout_change_during_read_is_rejected_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let writer = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    let result = lease.with_connection(&budget(), |conn| {
        let value: i64 = conn.query_row("SELECT count(*) FROM libraries", [], |r| r.get(0))?;
        writer.execute_batch("PRAGMA user_version=1")?;
        Ok(value)
    });
    assert!(result.is_err());
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
}

#[test]
fn cleanup_failure_cannot_publish_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    lease.inject_cleanup_failure();
    let result = lease.with_connection(&budget(), |conn| {
        Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)
    });
    assert!(matches!(
        result,
        Err(LibraryReadError::Store(crate::StoreError::Sqlite(_)))
    ));
    drop(lease);
    assert_eq!(pool.available(), 0);
}

#[test]
fn confirmed_layout_mismatch_closes_pool_after_cancellation() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let lease = pool.try_checkout().unwrap();
    let writer = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    writer.execute_batch("PRAGMA user_version=1").unwrap();
    let (hook_entered, entered) = TimedGate::new("layout validation paused");
    let (hook_release, release) = TimedGate::new("layout validation released");
    // Only the first `application_id` read pauses: the same authorizer also sees
    // the post-work re-check, which must run after the cancellation instead of
    // waiting at the gate a second time.
    let once = Arc::new(AtomicBool::new(false));
    let hook_once = Arc::clone(&once);
    lease
        .conn
        .as_ref()
        .unwrap()
        .authorizer(Some(move |ctx: AuthContext<'_>| {
            if matches!(
                ctx.action,
                AuthAction::Pragma {
                    pragma_name: "application_id",
                    ..
                }
            ) && !hook_once.swap(true, Ordering::AcqRel)
            {
                hook_entered.wait();
                hook_release.wait();
            }
            Authorization::Allow
        }));
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker = std::thread::spawn(move || {
        let mut lease = lease;
        lease.with_connection(
            &QueryBudget {
                deadline: Instant::now() + Duration::from_secs(3),
                cancelled: worker_cancelled,
            },
            |conn| Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?),
        )
    });
    entered.wait();
    cancelled.store(true, Ordering::Release);
    release.wait();
    assert!(worker.join().unwrap().is_err());
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
}

#[test]
fn post_work_validation_reduces_actual_sqlite_busy_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let (lease_entered, entered) = TimedGate::new("postcheck entered");
    let (lease_release, release) = TimedGate::new("postcheck released");
    let observed = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    *lease.pool.postcheck_gate.lock().unwrap() = Some((lease_entered, lease_release));
    *lease.pool.postcheck_timeout_ms.lock().unwrap() = Some(Arc::clone(&observed));
    let (deadline_tx, deadline_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        // The window starts where the work starts, so the post-check really sees
        // a nearly expired budget instead of one eaten by thread start-up.
        let budget = QueryBudget {
            deadline: Instant::now() + Duration::from_millis(600),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        deadline_tx.send(budget.deadline).unwrap();
        let _ = lease.with_connection(&budget, |conn| {
            Ok(conn.query_row("SELECT count(*) FROM libraries", [], |r| r.get::<_, i64>(0))?)
        });
    });
    let deadline = expect_from_worker(&deadline_rx, "its deadline");
    entered.wait(); // Read transaction has rolled back; postcheck has not begun.
    // Release shortly before the deadline: the postcheck must trim the busy
    // timeout to what is left (~25 ms), not keep the initial window.
    let release_at = deadline
        .checked_sub(Duration::from_millis(25))
        .unwrap_or(deadline);
    while Instant::now() < release_at {
        std::thread::sleep(Duration::from_millis(1));
    }
    release.wait();
    worker.join().unwrap();
    assert!(
        observed.load(Ordering::Acquire) <= 25,
        "postcheck kept the initial busy timeout"
    );
}

#[test]
fn diagnostic_scope_counts_rollback_and_restores_after_error_cancel_and_panic() {
    use crate::library_query::items::QueryWorkStats;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    for failure in ["error", "cancel", "panic"] {
        let budget = budget();
        let mut stats = QueryWorkStats::default();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lease.with_connection_instrumented(
                &budget,
                |conn| {
                    conn.query_row("SELECT count(*) FROM libraries", [], |row| {
                        row.get::<_, i64>(0)
                    })?;
                    match failure {
                        "panic" => panic!("fixture panic"),
                        "cancel" => {
                            budget.cancelled.store(true, Ordering::Release);
                            Ok(())
                        }
                        _ => {
                            conn.execute_batch("SELECT missing FROM missing")?;
                            Ok(())
                        }
                    }
                },
                Some(&mut stats),
            )
        }));
        assert!(outcome.is_err() || outcome.unwrap().is_err());
        assert!(
            stats.statements > 2 && stats.vm_steps > 0,
            "layout/BEGIN/read/rollback: {stats:?}"
        );
        assert!(
            lease
                .with_connection(&super::tests::budget(), |conn| Ok(!conn.is_autocommit()))
                .unwrap()
        );
    }
    drop(lease);
    assert_eq!(pool.available(), 1);
}
