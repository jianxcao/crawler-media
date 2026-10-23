//! C03: read-then-write store transactions must take the write lock up front.
//!
//! A DEFERRED transaction that reads, then waits for a concurrent writer,
//! fails with BUSY_SNAPSHOT once that writer commits. IMMEDIATE waits at BEGIN
//! and then sees the committed state.

use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use store::{RetentionClass, Store};

const CUTOFF_SECS: i64 = 1_700_000_000;

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

    let prune = std::thread::spawn(move || {
        store
            .prune_data_history(
                RetentionClass::TransientCatalog,
                CUTOFF_SECS * 1000,
                None,
                500,
                false,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    std::thread::sleep(Duration::from_millis(300));
    tx.commit().unwrap();
    let committed_prune = prune.join().unwrap();
    assert!(committed_prune.is_ok(), "prune: {committed_prune:?}");
    let remaining: i64 = Connection::open(tmp.path().join("cache.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM tmdb_cache", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "IMMEDIATE prune sees the concurrent insert");
}
