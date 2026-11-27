//! The instance-lease heartbeat stays responsive while the Library writer is held.
//!
//! The heartbeat uses the shared App writer independently of Library batches, so
//! a stalled Library write cannot let another instance take over the data directory.
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store, job_loop};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};

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
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
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
async fn heartbeat_survives_long_library_write() {
    fast_lease();
    let tmp = tempfile::tempdir().unwrap();
    let first_state = open(tmp.path());
    let locked_store = first_state.store();
    let first = job_loop::spawn_job_loop(first_state);
    assert!(!first.is_read_only(), "first instance must own the lease");
    let beat_on_acquire = heartbeat_at(tmp.path());

    // Hold the Library writer for 1 s: >3x the TTL.
    let holder = std::thread::spawn(move || {
        let _guard =
            crate::writer_hold::WriterHold::acquire(locked_store.clone(), store::Db::Library);
        std::thread::sleep(Duration::from_millis(1000));
    });
    std::thread::sleep(Duration::from_millis(600));

    let beat_under_lock = heartbeat_at(tmp.path());
    assert!(
        beat_under_lock > beat_on_acquire,
        "heartbeat must keep refreshing the lease while a batch holds the Library writer \
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_instance_defers_startup_writes_until_lease_acquired() {
    fast_lease();
    let tmp = tempfile::tempdir().unwrap();
    let first_state = open(tmp.path());
    let first_store = first_state.store();
    let first = job_loop::spawn_job_loop(first_state);
    assert!(!first.is_read_only(), "first instance must own the lease");

    // Hold Library writer AND own the lease in instance 1 so a standby instance
    // booting read-only must neither block on Library writes nor mutate startup state.
    let hold = crate::writer_hold::WriterHold::acquire(first_store.clone(), store::Db::Library);
    let data_dir = tmp.path().join("data");
    let stage_dir = tmp.path().join("stage");
    let second_state = tokio::time::timeout(
        Duration::from_millis(400),
        tokio::task::spawn_blocking(move || {
            let store = Arc::new(Store::open(&data_dir).unwrap());
            let initial_lease = api::instance_lease::acquire(&store);
            assert!(initial_lease.is_err(), "standby must see lease conflict");
            let action = api::bootstrap_credentials::prepare_admin_credentials(
                store.as_ref(),
                "lease-secret",
                None,
                api::bootstrap_credentials::BootstrapMode::TestFixture,
            )
            .unwrap();
            let state = ApiState::new_arc_with_admin_bootstrap_writable(
                store,
                RegistrySnapshot::builtin().unwrap(),
                Arc::new(NoFetch),
                Arc::new(MemoryDownloader::new(stage_dir)),
                action,
                false,
            )
            .unwrap();
            (state, initial_lease)
        }),
    )
    .await
    .expect("standby ApiState startup must not block on Library writer")
    .unwrap();
    let (second_state, initial_lease) = second_state;
    assert!(
        !second_state.instance_writable(),
        "standby ApiState must start non-writable"
    );

    drop(hold);
    first.shutdown().await.unwrap();

    let second = job_loop::spawn_job_loop_with_lease(second_state.clone(), initial_lease);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if !second.is_read_only() && second_state.instance_writable() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        !second.is_read_only() && second_state.instance_writable(),
        "standby instance must transition to writable after acquiring lease"
    );
    assert!(
        second_state
            .store()
            .get_setting("jellyfin.server_id")
            .unwrap()
            .is_some(),
        "jellyfin.server_id must be persisted once writable"
    );
    second.shutdown().await.unwrap();
}
