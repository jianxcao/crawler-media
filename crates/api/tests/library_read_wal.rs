//! Active snapshot, legal writer, cancellation and post-worker checkpoint proof.
use api::router;
use axum::http::StatusCode;
use media_server::{provider::QueryTiming, query_metrics::QueryMetrics};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tower::ServiceExt;
#[path = "library_read/support.rs"]
mod support;

async fn pinned_snapshot(mut fixture: support::Fixture, cancel: bool) {
    let pool = fixture.state.library_read_pool_weak().upgrade().unwrap();
    let capacity = warm_readers(&pool);
    wait_for_initial_checkpoint(&fixture.database()).await;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    pool.set_snapshot_observer(Some(Arc::new(move || {
        entered_tx.send(()).unwrap();
        release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    })));
    let started = Instant::now();
    let timing = QueryTiming {
        started,
        deadline: started + Duration::from_millis(if cancel { 200 } else { 5000 }),
    };
    let metrics = QueryMetrics::new(timing, true);
    let request = support::request(
        &format!("/api/v1/libraries/{}/facets", fixture.library),
        timing,
        metrics.clone(),
    );
    let app = router(fixture.state.clone());
    let reader = tokio::spawn(app.clone().oneshot(request));
    let peer_metrics = QueryMetrics::new(timing, true);
    let peer_request = support::request(
        &format!("/api/v1/libraries/{}/facets", fixture.library),
        timing,
        peer_metrics.clone(),
    );
    let peer = tokio::spawn(app.oneshot(peer_request));
    let entered = tokio::task::spawn_blocking(move || {
        entered_rx.recv_timeout(Duration::from_secs(2))?;
        entered_rx.recv_timeout(Duration::from_secs(2))
    })
    .await
    .unwrap();
    assert!(
        entered.is_ok(),
        "reader must pause after an actual Library read establishes its snapshot"
    );
    let state = fixture.state.clone();
    let mut files = fixture.files.clone();
    let writer = tokio::task::spawn_blocking(move || support::write_batch(&state, &mut files, 1))
        .await
        .unwrap();
    assert!(writer.0 > 0.);
    let pinned = support::checkpoint(&fixture.database(), "PASSIVE");
    assert!(
        pinned.1 > pinned.2,
        "live snapshot must prevent complete checkpoint: {pinned:?}"
    );
    let wal = std::fs::metadata(format!("{}-wal", fixture.database().display()))
        .unwrap()
        .len();
    if cancel {
        let response = tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            pool.available(),
            capacity - 2,
            "HTTP timeout cannot return a busy connection"
        );
        assert_eq!(
            fixture.state.http_work.available_permits(),
            6,
            "late worker retains its permit"
        );
        assert_eq!(
            peer.await.unwrap().unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        pool.set_snapshot_observer(None);
        let released = Instant::now();
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        wait_for_worker(&fixture.state, &pool, capacity).await;
        let recovered = support::checkpoint(&fixture.database(), "TRUNCATE");
        assert_eq!(recovered, (0, 0, 0));
        let s = metrics.snapshot();
        assert_eq!(s.timeout_stage, Some("lease"));
        assert!(s.worker_stop_delay_ms.is_some());
        println!(
            "wal_cancel pinned={pinned:?} wal_file_bytes={wal} postexit={recovered:?} release_to_worker_stop_ms={:.3} metrics={s:?}",
            released.elapsed().as_secs_f64() * 1000.
        );
    } else {
        pool.set_snapshot_observer(None);
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(reader.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(peer.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(
            support::checkpoint(&fixture.database(), "TRUNCATE"),
            (0, 0, 0)
        );
        println!("wal_writer_commit pinned={pinned:?} wal_file_bytes={wal}");
    }
    assert_readers_returned(&pool, capacity);
    fixture.files.clear();
}

fn warm_readers(pool: &store::ReaderPool) -> usize {
    let capacity = 8;
    assert_readers_returned(pool, capacity);
    capacity
}

fn assert_readers_returned(pool: &store::ReaderPool, capacity: usize) {
    let readers = (0..capacity)
        .map(|_| pool.try_checkout().unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        pool.try_checkout(),
        Err(store::LibraryReadError::Busy)
    ));
    drop(readers);
    assert_eq!(pool.available(), capacity);
}

async fn wait_for_initial_checkpoint(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let path = path.to_path_buf();
        let result = tokio::task::spawn_blocking(move || support::checkpoint(&path, "TRUNCATE"))
            .await
            .unwrap();
        if result == (0, 0, 0) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "initial checkpoint remained busy: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn wait_for_worker(state: &api::ApiState, pool: &store::ReaderPool, capacity: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while pool.available() != capacity || state.http_work.available_permits() != 8 {
        assert!(
            Instant::now() < deadline,
            "worker must finish rollback before capacity returns"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}
// Construct outside Tokio so unrelated background drivers cannot borrow the
// unified Library pool while this fixture proves ownership of every lease.
fn snapshot_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn active_snapshot_allows_100_file_writer_commit() {
    let fixture = support::Fixture::new(100);
    snapshot_runtime().block_on(pinned_snapshot(fixture, false));
}
#[test]
fn cancelled_snapshot_returns_only_after_worker_cleanup_then_checkpoint_recovers() {
    let fixture = support::Fixture::new(100);
    snapshot_runtime().block_on(pinned_snapshot(fixture, true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_checkpoint_waits_for_a_transient_snapshot() {
    let mut fixture = support::Fixture::new(1);
    let path = fixture.database();
    let snapshot = rusqlite::Connection::open(&path).unwrap();
    snapshot.execute_batch("BEGIN").unwrap();
    let _: i64 = snapshot
        .query_row("SELECT COUNT(*) FROM library_files", [], |row| row.get(0))
        .unwrap();
    support::write_batch(&fixture.state, &mut fixture.files, 1);
    assert_eq!(support::checkpoint(&path, "TRUNCATE").0, 1);
    let wait_path = path.clone();
    let waiter = tokio::spawn(async move { wait_for_initial_checkpoint(&wait_path).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let waited = !waiter.is_finished();
    snapshot.execute_batch("ROLLBACK").unwrap();
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(
        waited,
        "initial checkpoint must wait until the snapshot is released"
    );
}
