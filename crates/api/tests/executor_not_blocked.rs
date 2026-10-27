//! L2: blocking `Store` work must never run on a tokio worker thread.
//!
//! Every `/api/v1` request goes through `auth::authenticate`, and the job
//! handlers take the process-wide `Store` lock. When either of those blocks on
//! that lock the whole runtime stalls: nothing else scheduled on the same
//! executor runs, including timers and `/health`. A long batch (scan,
//! integrity) holds the lock for minutes, so the wait has to happen on the
//! blocking pool with a deadline that turns into 503 + retry.

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

/// State with one admin whose bearer token is usable over HTTP.
///
/// Built **outside** any Tokio runtime on purpose: `ApiState::new` only starts
/// its background drivers (probe workers, chapter frames, root reassignment,
/// …) when a runtime already exists, and those drivers take the `Store` lock on
/// their first poll. Leaving them out keeps the measurement about the request
/// path itself.
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
            login: "jobs-admin".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        guard.insert_user_with_password(&user, "admin-pass").unwrap();
        let token = "jobs-admin-session".to_string();
        guard.set_user_token(user.id, &token).unwrap();
        token
    };
    (state, token)
}

/// `GET /api/v1/jobs` while another thread holds the `Store` lock for 2 s.
#[test]
fn heartbeat_route_responds_while_store_locked() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let app = router(state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let locked_store = state.store();
    let holder = std::thread::spawn(move || {
        // A batch that holds the process-wide Store lock: scan / integrity work.
        let _guard = locked_store.lock();
        std::thread::sleep(Duration::from_millis(2000));
    });
    std::thread::sleep(Duration::from_millis(50));

    runtime.block_on(async move {
        let request_app = app.clone();
        let pending = tokio::spawn(async move {
            request_app
                .oneshot(request("GET", "/api/v1/jobs", Some(&token)))
                .await
        });

        // The executor must stay free while the request waits for the lock.
        let started = Instant::now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let slept = started.elapsed();
        assert!(
            slept < Duration::from_millis(200),
            "executor starved by a blocking Store lock: sleep(10ms) took {slept:?}"
        );

        // The wait itself is bounded: 503 (retry) instead of hanging on the lock.
        let response = pending.await.unwrap().unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a Store lookup that cannot get the lock in time must answer 503, not hang"
        );
    });
    holder.join().unwrap();
}
