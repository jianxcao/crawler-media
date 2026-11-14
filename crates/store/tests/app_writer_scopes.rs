//! Instrumentation belongs to the operation which acquired a shared connection.
//! A diagnostic/query scope must not own that connection while its callback waits.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use store::stats::StatsScope;
use store::{Db, LibraryReadError, QueryBudget, Store};

#[test]
fn app_diagnostics_exclude_concurrent_foreground_writer_work() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting("measured", "original").unwrap();
    let (_, baseline) = store.with_stats(StatsScope::App, || store.get_setting("measured"));
    assert!(baseline.statements > 0);
    let writers = store.writers();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let measured = std::thread::spawn(move || {
        store.with_stats(StatsScope::App, || {
            let value = store.get_setting("measured").unwrap();
            entered_tx.send(()).unwrap();
            let concurrent = completed_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap_or(false);
            (value, concurrent)
        })
    });
    entered_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("app read nested inside diagnostics deadlocked");
    let foreign = std::thread::spawn(move || {
        let result = writers.with_writer(Db::App, |conn| {
            conn.execute(
                "INSERT INTO settings(key,value) VALUES('unrelated','foreground')",
                [],
            )?;
            Ok(())
        });
        let _ = completed_tx.send(result.is_ok());
        result
    });
    let ((own_value, concurrent), observed) = measured.join().unwrap();
    foreign.join().unwrap().unwrap();
    assert!(
        concurrent,
        "diagnostic scope held the app writer across its callback"
    );
    assert_eq!(own_value.as_deref(), Some("original"));
    assert_eq!(
        observed.statements, baseline.statements,
        "foreign app work polluted diagnostics"
    );
}

#[test]
fn cancelling_query_budget_cannot_interrupt_an_unrelated_app_write() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting("budgeted", "original").unwrap();
    let writers = store.writers();
    let cancelled = Arc::new(AtomicBool::new(false));
    let budget = QueryBudget {
        deadline: Instant::now() + Duration::from_secs(10),
        cancelled: cancelled.clone(),
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let measured = std::thread::spawn(move || {
        let mut concurrent = false;
        let result = store.with_query_budget(&budget, |store| {
            assert_eq!(store.get_setting("budgeted")?.as_deref(), Some("original"));
            entered_tx.send(()).unwrap();
            concurrent = completed_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap_or(false);
            Ok(())
        });
        (store, result, concurrent)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("app read nested inside query budget deadlocked");
    cancelled.store(true, Ordering::Release);
    let foreign = std::thread::spawn(move || {
        let result = writers.with_writer(Db::App, |conn| {
            // Enough VM instructions to fire an accidentally inherited cancellation
            // hook. The durable setting is the observable result, not the SQL shape.
            conn.execute(
                "WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10000)
                 INSERT INTO settings(key,value) SELECT 'independent',sum(value) FROM n",
                [],
            )?;
            Ok(())
        });
        let _ = completed_tx.send(result.is_ok());
        result
    });
    let (store, result, concurrent) = measured.join().unwrap();
    foreign.join().unwrap().unwrap();
    assert!(matches!(result, Err(LibraryReadError::Cancelled)));
    assert!(
        concurrent,
        "query scope blocked or cancelled an unrelated app writer"
    );
    assert_eq!(
        store.get_setting("independent").unwrap().as_deref(),
        Some("50005000")
    );
    let fresh = QueryBudget {
        deadline: Instant::now() + Duration::from_secs(1),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let value = store
        .with_query_budget(&fresh, |store| Ok(store.get_setting("budgeted")?))
        .unwrap();
    assert_eq!(value.as_deref(), Some("original"));
}

#[test]
fn cancelled_app_mutation_waiting_for_writer_never_commits() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let held = std::thread::spawn(move || {
        writers
            .with_writer(Db::App, |_| {
                held_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let budget = QueryBudget {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: cancelled.clone(),
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let mutation = std::thread::spawn(move || {
        let result = store.with_query_budget(&budget, |store| {
            entered_tx.send(()).unwrap();
            store.put_setting("cancelled-mutation", "must not commit")?;
            Ok(())
        });
        (store, result)
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    cancelled.store(true, Ordering::Release);
    release_tx.send(()).unwrap();
    held.join().unwrap();
    let (store, result) = mutation.join().unwrap();
    assert!(matches!(result, Err(LibraryReadError::Cancelled)));
    assert_eq!(
        store.get_setting("cancelled-mutation").unwrap(),
        None,
        "an App mutation cancelled while waiting must not commit later"
    );
}

#[test]
fn expired_app_mutation_finishes_before_its_writer_is_released() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let held = std::thread::spawn(move || {
        writers
            .with_writer(Db::App, |_| {
                held_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let mutation = std::thread::spawn(move || {
        let budget = QueryBudget {
            deadline: Instant::now() + Duration::from_millis(150),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let result = store.with_query_budget(&budget, |store| {
            entered_tx.send(()).unwrap();
            store.put_setting("expired-mutation", "must not commit")?;
            Ok(())
        });
        let _ = completed_tx.send(());
        (store, result)
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let completed_before_release = completed_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    release_tx.send(()).unwrap();
    held.join().unwrap();
    let (store, result) = mutation.join().unwrap();
    assert!(matches!(result, Err(LibraryReadError::Deadline)));
    assert_eq!(
        store.get_setting("expired-mutation").unwrap(),
        None,
        "an App mutation whose budget expires while waiting must never commit"
    );
    assert!(
        completed_before_release,
        "App writer acquisition ignored the operation deadline"
    );
}
