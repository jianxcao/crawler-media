//! A returned reader cannot overtake a completed planner refresh.
use super::*;
use crate::{QueryBudget, test_support::gate::TimedGate};
use std::time::{Duration, Instant};

fn plan(lease: &mut ReaderLease) -> String {
    lease
        .with_connection(
            &QueryBudget {
                deadline: Instant::now() + Duration::from_secs(3),
                cancelled: Default::default(),
            },
            |conn| {
                Ok(conn.query_row(
                    "EXPLAIN QUERY PLAN SELECT payload FROM facts WHERE a=42 AND b=1",
                    [],
                    |row| row.get(3),
                )?)
            },
        )
        .unwrap()
}

#[test]
fn returning_reader_concurrent_with_invalidation_reloads_planner_statistics() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("planner.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
        CREATE TABLE facts(a INTEGER,b INTEGER,payload TEXT);
        CREATE INDEX by_a ON facts(a); CREATE INDEX by_b ON facts(b);
        WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10000)
        INSERT INTO facts SELECT 1,value,'payload' FROM n; ANALYZE;",
        )
        .unwrap();
    let pool =
        ReaderPool::open(path, NonZeroUsize::new(1).unwrap(), "planner", |_| Ok(())).unwrap();
    let mut lease = pool.try_checkout().unwrap();
    assert!(plan(&mut lease).contains("by_b"));
    writer
        .execute_batch(
            "DELETE FROM facts;
        WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10000)
        INSERT INTO facts SELECT value,1,'payload' FROM n; ANALYZE;",
        )
        .unwrap();
    assert!(
        plan(&mut lease).contains("by_b"),
        "fixture must retain stale per-connection statistics"
    );
    let (entered, worker_entered) = TimedGate::new("reader returning before idle insertion");
    let (release, worker_release) = TimedGate::new("reader return may insert idle connection");
    *pool.inner.return_gate.lock().unwrap() = Some((worker_entered, worker_release));
    let returning = std::thread::spawn(move || drop(lease));
    entered.wait();
    pool.invalidate_planner();
    *pool.inner.return_gate.lock().unwrap() = None;
    release.wait();
    returning.join().unwrap();
    let mut next = pool.try_checkout().unwrap();
    assert!(
        plan(&mut next).contains("by_a"),
        "a return racing a completed invalidation restored stale planner statistics"
    );
}
