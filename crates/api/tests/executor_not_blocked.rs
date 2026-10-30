//! L2: blocking `Store` work must never run on a tokio worker thread.
//!
//! Authentication and Jobs reads use their own read connections. Holding a Library
//! writer must leave both requests and unrelated executor timers responsive.
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}

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
/// …) when a runtime already exists. Leaving them out keeps the measurement about the request
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
        let guard = store.as_ref();
        let user = domain::User {
            id: domain::UserId::new(),
            login: "jobs-admin".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        guard
            .insert_user_with_password(&user, "admin-pass")
            .unwrap();
        let token = "jobs-admin-session".to_string();
        guard.set_user_token(user.id, &token).unwrap();
        token
    };
    (state, token)
}

/// `GET /api/v1/jobs` while another thread holds the Library writer for 2 s.
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
        // A scan or integrity batch holds the Library writer.
        let _guard =
            crate::writer_hold::WriterHold::acquire(locked_store.clone(), store::Db::Library);
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

        // The executor must stay free while the Library writer is held.
        let started = Instant::now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let slept = started.elapsed();
        assert!(
            slept < Duration::from_millis(200),
            "executor starved by a blocking Store lock: sleep(10ms) took {slept:?}"
        );

        let response = tokio::time::timeout(Duration::from_millis(500), pending)
            .await
            .expect("Jobs reader must finish while the Library writer is still held")
            .unwrap()
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Jobs reads must remain available while the Library writer is held"
        );
    });
    holder.join().unwrap();
}

#[test]
fn scan_wakeup_does_not_park_executor_on_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = state_with_admin(tmp.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = state.store();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::Library);
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    locked_rx.recv().unwrap();
    let elapsed = runtime.block_on(async {
        let started = Instant::now();
        state.scan_coordinator.trigger_next(&state);
        tokio::time::sleep(Duration::from_millis(10)).await;
        started.elapsed()
    });
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "scan wakeup parked executor for {elapsed:?}"
    );
}

#[test]
fn up_next_does_not_park_executor_on_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let app = router(state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = state.store();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::Library);
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    locked_rx.recv().unwrap();
    let (elapsed, response) = runtime.block_on(async {
        let started = Instant::now();
        let (response, elapsed) = tokio::join!(
            app.oneshot(request("GET", "/api/v1/playback/up-next", Some(&token))),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            },
        );
        (elapsed, response.unwrap())
    });
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "up-next parked executor for {elapsed:?}"
    );
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn episode_routes_do_not_park_executor_on_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let library = state
        .store()
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap()
        .id;
    let media = domain::MediaId::new();
    for uri in [
        format!("/api/v1/playback/items/{media}/episodes?season_number=1"),
        format!("/api/v1/libraries/{library}/items/{media}/episodes?season=1"),
    ] {
        assert_eq!(
            assert_request_timer_responsive(&state, &token, "GET", &uri),
            StatusCode::NOT_FOUND
        );
    }
}

#[test]
fn scan_write_routes_do_not_park_executor_on_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let library = state
        .store()
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap()
        .id;
    for (uri, expected) in [
        (
            format!("/api/v1/libraries/{library}/scan"),
            StatusCode::ACCEPTED,
        ),
        (
            format!("/api/v1/libraries/{library}/scan-operations/missing/cancel"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(
            assert_request_timer_responsive(&state, &token, "POST", &uri),
            expected
        );
    }
}

fn assert_request_timer_responsive(
    state: &ApiState,
    token: &str,
    method: &str,
    uri: &str,
) -> StatusCode {
    let app = router(state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = state.store();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::Library);
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    locked_rx.recv().unwrap();
    let (response, elapsed) = runtime.block_on(async {
        let started = Instant::now();
        let (response, elapsed) =
            tokio::join!(app.oneshot(request(method, uri, Some(token))), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            });
        (response.unwrap(), elapsed)
    });
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "{uri} parked executor for {elapsed:?}"
    );
    response.status()
}
