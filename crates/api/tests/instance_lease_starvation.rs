//! L1: the instance-lease heartbeat must not queue behind the process-wide
//! `Arc<parking_lot::Mutex<Store>>` lock.
//!
//! A long batch (integrity / scan) holds that lock for minutes. When the
//! heartbeat shares it, the lease stops being refreshed, its TTL expires and a
//! second instance takes the data directory over — two writers on one
//! directory. The heartbeat therefore runs on its own app.db connection.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store, job_loop};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn open(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "lease-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

/// Timers shrink to ~1/100 of production so the test spans milliseconds.
/// The TTL hook exists so the takeover window stays below the lock hold; the
/// production default stays 60 s.
fn fast_lease() {
    // SAFETY: tests only shorten timers; every reader tolerates any value.
    unsafe {
        std::env::set_var("CRAWLER_MEDIA_LEASE_HEARTBEAT_MS", "50");
        std::env::set_var("CRAWLER_MEDIA_LEASE_RETRY_MS", "50");
        std::env::set_var("CRAWLER_MEDIA_LEASE_TTL_MS", "300");
    }
}

/// Read the lease row the way a competing process would: through its own
/// connection, never through `ApiState::store`.
fn heartbeat_at(root: &Path) -> i64 {
    let conn = rusqlite::Connection::open(root.join("data").join("app.db")).unwrap();
    conn.query_row(
        "SELECT heartbeat_at FROM instance_lock WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_survives_long_store_lock() {
    fast_lease();
    let tmp = tempfile::tempdir().unwrap();
    let first_state = open(tmp.path());
    let locked_store = first_state.store();
    let first = job_loop::spawn_job_loop(first_state);
    assert!(!first.is_read_only(), "first instance must own the lease");
    let beat_on_acquire = heartbeat_at(tmp.path());

    // A batch that holds the process-wide Store lock for 1 s: >3x the TTL.
    let holder = std::thread::spawn(move || {
        let _guard = locked_store.lock();
        std::thread::sleep(Duration::from_millis(1000));
    });
    std::thread::sleep(Duration::from_millis(600));

    let beat_under_lock = heartbeat_at(tmp.path());
    assert!(
        beat_under_lock > beat_on_acquire,
        "heartbeat must keep refreshing the lease while a batch holds the Store lock \
         (acquired at {beat_on_acquire}, still {beat_under_lock} after 600 ms)"
    );

    // The lease is still fresh, so a second instance must not take it over.
    let second = job_loop::spawn_job_loop(open(tmp.path()));
    assert!(
        second.is_read_only(),
        "second instance must not take over a lease the first one still heartbeats"
    );
    assert!(!first.is_read_only(), "first instance still owns the lease");

    holder.join().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !first.is_read_only(),
        "the lease survives the whole lock hold"
    );
    second.shutdown().await.unwrap();
    first.shutdown().await.unwrap();
}
