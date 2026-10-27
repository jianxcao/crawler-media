//! Task 22 slice 1: playback and search reads must not wait for the `Store` lock
//! on a tokio worker.
//!
//! A long batch (scan, integrity) holds the process-wide `Store` mutex for
//! seconds. If a request parks its tokio worker on that mutex, every other task
//! scheduled on the runtime is delayed — including `/health` and requests that
//! never touch the store. The lock is waited for on the blocking pool with a
//! bounded deadline (`http::store_wait`), and a lock that stays contended answers
//! 503 + `Retry-After` instead of hanging.

use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::UserRole;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn request(method: &str, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).unwrap()
}

/// State with one admin whose bearer token is usable over HTTP, built **outside**
/// any Tokio runtime so the background drivers `ApiState::new` starts (probe
/// workers, chapter frames, root reassignment) do not take the lock themselves.
fn state_with_admin(root: &std::path::Path) -> (ApiState, String) {
    let state = ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "executor-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    let token = {
        let store = state.store();
        let guard = store.lock();
        let user = domain::User {
            id: domain::UserId::new(),
            login: "slice-admin".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        guard
            .insert_user_with_password(&user, "admin-pass")
            .unwrap();
        let token = "slice-admin-session".to_string();
        guard.set_user_token(user.id, &token).unwrap();
        token
    };
    (state, token)
}

fn busy_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(value["error"]["code"].as_str()?.to_string())
}

/// Reads may wait through a batch shorter than the five-second read budget.
#[test]
fn playback_and_search_reads_succeed_after_a_long_store_batch() {
    check_contended_reads(Some(Duration::from_millis(2500)), StatusCode::OK);
}

/// A batch that outlasts the budget must get 503, with the executor still free.
#[test]
fn playback_and_search_reads_time_out_without_blocking_the_executor() {
    check_contended_reads(None, StatusCode::SERVICE_UNAVAILABLE);
}

fn check_contended_reads(release_after: Option<Duration>, expected: StatusCode) {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let app = router(state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let held_store = state.store();
    let (locked, acquired) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        // A batch that holds the process-wide Store lock: scan / integrity work.
        let _guard = held_store.lock();
        locked.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(8)).unwrap();
    });
    acquired.recv_timeout(Duration::from_secs(1)).unwrap();

    runtime.block_on(async move {
        let reads = async {
            let playback =
                app.clone()
                    .oneshot(request("GET", "/api/v1/playback/policy", Some(&token)));
            let search =
                app.clone()
                    .oneshot(request("GET", "/api/v1/search/history", Some(&token)));
            let started = Instant::now();
            let (playback, search) = tokio::join!(playback, search);
            let waited = started.elapsed();
            assert!(
                waited < Duration::from_secs(6),
                "reads exceeded the five-second budget plus scheduling margin: {waited:?}"
            );
            for (uri, response) in [
                ("/api/v1/playback/policy", playback.unwrap()),
                ("/api/v1/search/history", search.unwrap()),
            ] {
                assert_read_response(uri, response, expected).await;
            }
        };
        let heartbeat = async {
            // Check responsiveness during contention, before either request ends.
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(10)).await;
            let slept = started.elapsed();
            assert!(
                slept < Duration::from_millis(200),
                "executor starved by a blocking Store lock: sleep(10ms) took {slept:?}"
            );
            if let Some(delay) = release_after {
                tokio::time::sleep(delay).await;
                release.send(()).unwrap();
            }
        };
        tokio::join!(reads, heartbeat);
        if release_after.is_none() {
            release.send(()).unwrap();
        }
    });
    holder.join().unwrap();
}

async fn assert_read_response(uri: &str, response: axum::response::Response, expected: StatusCode) {
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert_eq!(
        status,
        expected,
        "{uri}: {}",
        String::from_utf8_lossy(&body)
    );
    if expected == StatusCode::SERVICE_UNAVAILABLE {
        assert_eq!(
            busy_code(&body).as_deref(),
            Some("runtime.busy"),
            "{uri}: {}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(
            headers
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1"),
            "{uri} must tell the client to retry"
        );
    }
}

#[test]
fn metadata_language_requests_leave_the_executor_responsive() {
    for method in ["GET", "PATCH"] {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (state, _) = runtime.block_on(async { state_with_admin(tmp.path()) });
        let app = router(state.clone());
        let (locked, acquired) = std::sync::mpsc::channel();
        let held = state.store();
        let holder = std::thread::spawn(move || {
            let _guard = held.lock();
            locked.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(800));
        });
        acquired.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut req = request(
            method,
            "/api/v1/settings/metadata-language",
            Some("executor-secret"),
        );
        if method == "PATCH" {
            req.headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            *req.body_mut() = Body::from(r#"{"expected_revision":0,"language":"ja"}"#);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let measured = std::thread::spawn(move || {
            let elapsed = runtime.block_on(async {
                let timer = async {
                    let start = Instant::now();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    start.elapsed()
                };
                let (elapsed, response) = tokio::join!(timer, app.oneshot(req));
                assert!(response.unwrap().status().is_success());
                elapsed
            });
            tx.send(elapsed).unwrap();
        });
        let elapsed = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("executor watchdog");
        holder.join().unwrap();
        measured.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(200),
            "{method} parked timer for {elapsed:?}"
        );
    }
}
