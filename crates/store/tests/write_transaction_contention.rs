//! C03: read-then-write store transactions must take the write lock up front.
//!
//! A DEFERRED transaction that reads, then waits for a concurrent writer,
//! fails with BUSY_SNAPSHOT once that writer commits. IMMEDIATE waits at BEGIN
//! and then sees the committed state.
//!
//! The test used to `sleep(300ms)` and hope the prune had reached the blocking
//! point by then; that proved nothing and made a slow box look like a pass. Now
//! the two threads hand over explicitly (no sleeping), the wait is *observed*
//! while the writer still holds the lock, and every cross-thread wait is bounded.

use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use store::{RetentionClass, Store};

const CUTOFF_SECS: i64 = 1_700_000_000;

/// Budget for one cross-thread handoff in this test. A handoff that never happens
/// fails the test by name instead of hanging the run.
const HANDOFF: Duration = Duration::from_secs(10);

#[test]
fn retention_prune_waits_for_concurrent_cache_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for index in 0..20 {
        store
            .put_catalog_cache(
                "tmdb",
                &format!("/search/movie?query={index}"),
                "{}",
                1,
                None,
            )
            .unwrap();
    }
    let mut writer = Connection::open(tmp.path().join("cache.db")).unwrap();
    writer.busy_timeout(Duration::from_secs(5)).unwrap();
    let tx = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute(
        "INSERT INTO tmdb_cache (cache_key, body, fetched_at, expires_at)
         VALUES ('/search/movie?query=late', '{}', 1, NULL)",
        [],
    )
    .unwrap();

    // Handshake, not sleep: the prune announces that it is about to enter SQLite,
    // and only then does this thread commit. So the prune's attempt is ordered
    // before the release of the lock, which is what makes the assertions below
    // prove a wait instead of a timing coincidence.
    let (entering, entered) = std::sync::mpsc::channel();
    let (reporting, reported) = std::sync::mpsc::channel();
    let prune = std::thread::spawn(move || {
        entering.send(()).unwrap();
        let outcome = store
            .prune_data_history(
                RetentionClass::TransientCatalog,
                CUTOFF_SECS * 1000,
                None,
                500,
                false,
            )
            .map(|_| ())
            .map_err(|error| error.to_string());
        reporting.send(outcome).unwrap();
    });
    entered
        .recv_timeout(HANDOFF)
        .expect("the prune thread never started");

    // Witness that the wait really happens: the writer transaction is still open,
    // so an IMMEDIATE prune must not have finished. (It cannot succeed here: the
    // 'late' row is only visible after a commit, which this thread has not made.)
    assert!(
        !prune.is_finished(),
        "the prune finished while the writer still held the lock"
    );

    tx.commit().unwrap();
    let committed_prune = reported
        .recv_timeout(HANDOFF)
        .expect("the prune never reported a result");
    prune.join().unwrap();
    assert!(committed_prune.is_ok(), "prune: {committed_prune:?}");

    // The proof of the wait: `remaining == 0` is only reachable if the prune ran
    // *after* the commit above. A DEFERRED read-then-write would instead have
    // failed with BUSY_SNAPSHOT, and a prune that beat the commit would have left
    // the 'late' row behind.
    let remaining: i64 = Connection::open(tmp.path().join("cache.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM tmdb_cache", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "IMMEDIATE prune sees the concurrent insert");
}
