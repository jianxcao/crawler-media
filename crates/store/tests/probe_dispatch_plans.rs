//! The dispatch refill must reach the queued window through an index, not a sort.
//!
//! `Store::pending_probe_dispatch_ids` runs on the dispatcher's hot path: once per completed
//! unit, for both windows. Its filter is served by `probe_unit_dispatch(stage,status,job_id)`
//! (partial, `WHERE status='queued'`), so the row *selection* is indexed — the danger is the
//! `ORDER BY`. Ordering by `probe_jobs.created_at_ms` has no index, so SQLite materialises and
//! sorts every queued unit on every refill: measured 30,339 VM steps for 2,000 queued units
//! and 300,339 for 20,000, i.e. O(N) per refill and O(N²/window) for one backlog drain.
//!
//! These cases pin the plan and the cost as deterministic counters (VM steps and
//! `EXPLAIN QUERY PLAN` text), never wall-clock. Before the fix the first case fails on the
//! `TEMP B-TREE` term and the second fails the absolute bound.

use rusqlite::Connection;
use store::{ProbeDispatchStage, Store};

/// Seeded backlog size and the refill limit the assertions use.
const BACKLOG: usize = 2_000;
const MORE_BACKLOG: usize = 18_000;
const LIMIT: usize = 64;

fn open_store() -> (tempfile::TempDir, Store, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let conn = Connection::open(tmp.path().join("data/library.db")).unwrap();
    (tmp, store, conn)
}

/// Seed `count` queued metadata jobs with one queued unit each, starting at `first`.
///
/// Raw SQL in one transaction: this case is about the *read* plan, and going through
/// `create_probe_job` would spend a write transaction per row without changing the shape the
/// plan is decided on. `scope_key` is unique because `probe_active_scope` makes it unique
/// among queued/running jobs.
fn seed_queued_jobs(conn: &Connection, first: usize, count: usize) {
    conn.execute_batch("BEGIN").unwrap();
    for index in first..first + count {
        let job = format!("job-{index:06}");
        conn.execute(
            "INSERT INTO probe_jobs (id, kind, media_id, season, scope_key, status, total,
                 created_at_ms)
             VALUES (?1, 'media_probe', 'media', NULL, ?2, 'queued', 1, ?3)",
            rusqlite::params![job, format!("scope-{index:06}"), 1_000 + index as i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO probe_job_units (job_id, file_id, kind, status, stage)
             VALUES (?1, ?2, 'movie', 'queued', 'metadata')",
            rusqlite::params![job, format!("ledger-{index:06}")],
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
}

fn plan_text(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let args: Vec<String> = (0..stmt.parameter_count()).map(|_| "x".to_string()).collect();
    stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
        row.get::<_, String>(3)
    })
    .unwrap()
    .map(Result::unwrap)
    .collect::<Vec<_>>()
    .join(" | ")
}

#[test]
fn dispatch_refill_sorts_nothing_and_scans_no_journal_table() {
    let (_tmp, _store, conn) = open_store();
    seed_queued_jobs(&conn, 0, BACKLOG);
    let text = plan_text(&conn, store::PENDING_DISPATCH_SQL);
    assert!(
        !text.contains("TEMP B-TREE"),
        "the refill must not sort the queued set: {text}"
    );
    assert!(
        text.contains("probe_unit_dispatch"),
        "the refill must reach the dispatch index: {text}"
    );
    for table in ["probe_jobs", "probe_job_units"] {
        assert!(
            !text.contains(&format!("SCAN {table}")),
            "{table} must be searched through an index, not scanned: {text}"
        );
    }
}

/// The whole point: refill cost is set by the window, not by the backlog.
///
/// Before the fix the same call cost 30,339 VM steps at 2,000 queued units and 300,339 at
/// 20,000; one backlog drain at the 64-wide window therefore visited O(N²) rows.
#[test]
fn dispatch_refill_cost_does_not_grow_with_the_backlog() {
    let (_tmp, store, conn) = open_store();
    seed_queued_jobs(&conn, 0, BACKLOG);
    let (ids, small_steps) = store
        .pending_probe_dispatch_ids_with_vm_steps(ProbeDispatchStage::Metadata, LIMIT)
        .unwrap();
    assert_eq!(ids.len(), LIMIT, "a full window must be filled");

    seed_queued_jobs(&conn, BACKLOG, MORE_BACKLOG);
    let (more_ids, large_steps) = store
        .pending_probe_dispatch_ids_with_vm_steps(ProbeDispatchStage::Metadata, LIMIT)
        .unwrap();
    assert_eq!(more_ids.len(), LIMIT);

    // 10x the backlog must not cost more than a bounded constant: the read touches the
    // window, not the queue. 64 index entries + 64 job probes is a few hundred steps; the
    // bound leaves room for plan details without hiding an O(backlog) scan.
    assert!(
        small_steps <= 4_000,
        "refill cost {small_steps} steps for {BACKLOG} queued units is not index-bounded"
    );
    assert!(
        large_steps <= 4_000,
        "refill cost {large_steps} steps for {} queued units is not index-bounded",
        BACKLOG + MORE_BACKLOG
    );
    assert!(
        large_steps <= small_steps * 2,
        "refill cost grew with the backlog: {small_steps} -> {large_steps} steps"
    );
}

/// Sibling read on the same table (`Store::probe_job_units`): one job's units are keyed by
/// `(job_id, file_id)`, so file order is free and rowid order is not.
#[test]
fn one_jobs_units_are_read_in_key_order_without_a_sort() {
    let (_tmp, store, conn) = open_store();
    seed_queued_jobs(&conn, 0, 1);
    conn.execute_batch("BEGIN").unwrap();
    for unit in 0..300 {
        conn.execute(
            "INSERT INTO probe_job_units (job_id, file_id, kind, status, stage)
             VALUES ('job-000000', ?1, 'movie', 'queued', 'metadata')",
            [format!("ledger-extra-{unit:04}")],
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();

    let text = plan_text(
        &conn,
        "SELECT job_id, file_id FROM probe_job_units WHERE job_id = 'job-000000' \
         ORDER BY file_id",
    );
    assert!(
        !text.contains("TEMP B-TREE"),
        "one job's units are read in key order, not sorted: {text}"
    );
    let units = store.probe_job_units("job-000000").unwrap();
    assert_eq!(units.len(), 301);
}
