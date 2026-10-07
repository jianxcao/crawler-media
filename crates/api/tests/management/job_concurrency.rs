use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use std::{path::Path, sync::atomic::AtomicUsize};

use api::{ApiState, Store, router, spawn_job_loop_with};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

use super::common::{create_site, create_subscribe, nexusphp, request};

struct BlockingFetcher {
    started: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
    blocked_once: AtomicBool,
}

impl Fetcher for BlockingFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        if !self.blocked_once.swap(true, Ordering::SeqCst) {
            if let Some(started) = self.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2));
        }
        Ok(nexusphp().to_string())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jobs_endpoint_stays_responsive_while_a_job_is_running() {
    let tmp = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let fetcher = Arc::new(BlockingFetcher {
        started: Mutex::new(Some(started_tx)),
        release: Mutex::new(release_rx),
        blocked_once: AtomicBool::new(false),
    });
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state.clone());
    create_site(&app).await;
    create_subscribe(&app, "search").await;

    let now = Arc::new(AtomicI64::new(1));
    let clock = now.clone();
    let loop_handle = spawn_job_loop_with(state, Duration::from_millis(5), move || {
        clock.fetch_add(30, Ordering::SeqCst)
    });
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(2)))
        .await
        .unwrap()
        .expect("Subscribe search did not start");

    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let _ = release_tx.send(());
    });
    let started = Instant::now();
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/jobs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let elapsed = started.elapsed();

    release.join().unwrap();
    loop_handle.abort();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_millis(150),
        "GET /api/v1/jobs waited {elapsed:?} for the running Job"
    );
}

struct RetryAfterTimeoutFetcher {
    started: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    retried: mpsc::Sender<()>,
    calls: std::sync::atomic::AtomicUsize,
}

struct RetryFixture {
    state: ApiState,
    started_rx: mpsc::Receiver<()>,
    release_tx: mpsc::Sender<()>,
    retried_rx: mpsc::Receiver<()>,
    now: Arc<AtomicI64>,
}

async fn retry_fixture(root: &Path) -> RetryFixture {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (retried_tx, retried_rx) = mpsc::channel();
    let fetcher = Arc::new(RetryAfterTimeoutFetcher {
        started: started_tx,
        release: Mutex::new(release_rx),
        retried: retried_tx,
        calls: AtomicUsize::new(0),
    });
    let state = ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        fetcher,
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    let app = router(state.clone());
    let site = create_site(&app).await;
    let site_id = site["id"].as_str().unwrap();
    let changed = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/sites/{site_id}"),
            Some("management-secret"),
            serde_json::json!({"rate_limit_per_minute": 60_000}),
        ))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    create_subscribe(&app, "search").await;
    RetryFixture {
        state,
        started_rx,
        release_tx,
        retried_rx,
        now: Arc::new(AtomicI64::new(1)),
    }
}

async fn assert_retry_waits(
    clock: &AtomicI64,
    retried: &mpsc::Receiver<()>,
    now: i64,
    message: &str,
) {
    clock.store(now, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(retried.try_recv().is_err(), "{message}");
}

impl Fetcher for RetryAfterTimeoutFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = self.started.send(());
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2));
        } else {
            let _ = self.retried.send(());
        }
        Err(IndexerError::Fetch("temporary failure".into()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_search_waits_for_old_attempt_and_retries_after_failure_backoff() {
    let tmp = tempfile::tempdir().unwrap();
    let RetryFixture {
        state,
        started_rx,
        release_tx,
        retried_rx,
        now,
    } = retry_fixture(tmp.path()).await;
    let clock = now.clone();
    let loop_handle = spawn_job_loop_with(state, Duration::from_millis(20), move || {
        clock.load(Ordering::SeqCst)
    });
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(2)))
        .await
        .unwrap()
        .expect("initial search did not start");

    // Pass the 120-second job timeout and the retry deadline while the original
    // blocking fetch is still alive. It must retain its claim and concurrency key.
    assert_retry_waits(
        &now,
        &retried_rx,
        302,
        "recovery overlapped the live attempt",
    )
    .await;
    assert_retry_waits(&now, &retried_rx, 332, "retry overlapped the live attempt").await;

    let _ = release_tx.send(());
    // The failed attempt completes at t=332, so its 30-second backoff ends at 362.
    assert_retry_waits(
        &now,
        &retried_rx,
        332,
        "backoff used the attempt start time",
    )
    .await;
    assert_retry_waits(
        &now,
        &retried_rx,
        361,
        "retry started before backoff elapsed",
    )
    .await;
    now.store(362, Ordering::SeqCst);
    let retry =
        tokio::task::spawn_blocking(move || retried_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .unwrap();
    loop_handle.abort();
    assert!(
        retry.is_ok(),
        "failed timed-out search was not retried after its backoff"
    );
}
