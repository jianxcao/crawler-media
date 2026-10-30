//! A planner refresh replaces idle readers and retires readers still on loan.
use rusqlite::Connection;
use std::{
    num::NonZeroUsize,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use store::{QueryBudget, ReaderLease, ReaderPool};
fn plan(lease: &mut ReaderLease) -> String {
    lease
        .with_connection(
            &QueryBudget {
                deadline: Instant::now() + Duration::from_secs(3),
                cancelled: Arc::new(AtomicBool::new(false)),
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
fn planner_epoch_reloads_statistics_for_idle_and_previously_leased_readers() {
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
        ReaderPool::open(path, NonZeroUsize::new(2).unwrap(), "planner", |_| Ok(())).unwrap();
    let mut idle = pool.try_checkout().unwrap();
    let mut leased = pool.try_checkout().unwrap();
    assert!(plan(&mut idle).contains("by_b"));
    assert!(plan(&mut leased).contains("by_b"));
    drop(idle);
    writer
        .execute_batch(
            "DELETE FROM facts;
        WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10000)
        INSERT INTO facts SELECT value,1,'payload' FROM n; ANALYZE;",
        )
        .unwrap();
    assert!(
        plan(&mut leased).contains("by_b"),
        "fixture must expose SQLite's stale per-connection planner statistics"
    );
    pool.invalidate_planner();
    drop(leased);
    let mut fresh_idle = pool.try_checkout().unwrap();
    let mut fresh_leased = pool.try_checkout().unwrap();
    assert!(
        plan(&mut fresh_idle).contains("by_a"),
        "idle reader kept pre-refresh statistics"
    );
    assert!(
        plan(&mut fresh_leased).contains("by_a"),
        "reader returned after refresh kept its old planner"
    );
}
