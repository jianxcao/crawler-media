//! Active snapshot, legal writer, cancellation and post-worker checkpoint proof.
use api::router;
use axum::http::StatusCode;
use media_server::{provider::QueryTiming, query_metrics::QueryMetrics};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tower::ServiceExt;
#[path = "library_read/support.rs"]
mod support;

async fn pinned_snapshot(cancel: bool) {
    let mut fixture = support::Fixture::new(100);
    let pool = fixture.state.library_read_pool_weak().upgrade().unwrap();
    assert_eq!(support::checkpoint(&fixture.database(), "TRUNCATE").0, 0);
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
            0,
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
        wait_for_worker(&fixture.state, &pool).await;
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
    fixture.files.clear();
}
async fn wait_for_worker(state: &api::ApiState, pool: &store::LibraryReadPool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while pool.available() != 2 || state.http_work.available_permits() != 8 {
        assert!(
            Instant::now() < deadline,
            "worker must finish rollback before capacity returns"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_snapshot_allows_100_file_writer_commit() {
    pinned_snapshot(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_snapshot_returns_only_after_worker_cleanup_then_checkpoint_recovers() {
    pinned_snapshot(true).await;
}
