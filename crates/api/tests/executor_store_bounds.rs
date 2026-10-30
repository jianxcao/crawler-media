//! Playback and search reads stay responsive while a Library writer is busy.
//!
//! Requests use read-only connections, so a scan or integrity write transaction
//! must leave read responses, `/health`, and unrelated executor timers responsive.
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

/// State with one admin whose bearer token is usable over HTTP, built **outside**
/// any Tokio runtime so the background drivers `ApiState::new` starts (probe
/// workers, chapter frames, root reassignment) do not interfere with the measurement.
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

/// L1 read/write split: request reads run on read-only connections and never wait
/// for the Library writer, however long a batch holds it (and even while a
/// write transaction is open on library.db). The detail page is the acceptance
/// endpoint: a missing item answers 404 at idle speed, never 503.
#[test]
fn reads_do_not_wait_for_the_writer_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let library = state
        .store()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .expect("default movie library")
        .id;
    let app = router(state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let held_store = state.store();
    let library_db = tmp.path().join("data/library.db");
    let (locked, acquired) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        // A long scan / scrape / probe batch: writer lock + open write transaction.
        let _guard =
            crate::writer_hold::WriterHold::acquire(held_store.clone(), store::Db::Library);
        let writer = rusqlite::Connection::open(library_db).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(20)).unwrap();
        writer.execute_batch("ROLLBACK").unwrap();
    });
    acquired.recv_timeout(Duration::from_secs(2)).unwrap();

    let detail = format!(
        "/api/v1/libraries/{library}/items/{}",
        domain::MediaId::new()
    );
    runtime.block_on(async {
        for (uri, expected) in [
            ("/api/v1/playback/policy", StatusCode::OK),
            ("/api/v1/search/history", StatusCode::OK),
            (detail.as_str(), StatusCode::NOT_FOUND),
        ] {
            let started = Instant::now();
            let response = app.clone().oneshot(request("GET", uri, Some(&token))).await;
            let waited = started.elapsed();
            assert_read_response(uri, response.unwrap(), expected).await;
            assert!(
                waited < Duration::from_millis(500),
                "{uri} waited {waited:?} behind the writer lock"
            );
        }
    });
    release.send(()).unwrap();
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
            let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
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

/// Detail / files / probe-status must stay on the reader pool even when they
/// report `probe_queued` / marker-refresh state. Holding the writer lock plus an
/// open write transaction must not make any of them wait.
#[test]
fn series_detail_and_files_answer_while_the_writer_is_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let (library_id, media_id) = {
        let store = state.store();
        let guard = store.as_ref();
        let library = guard
            .default_library(domain::MediaKind::Tv)
            .unwrap()
            .expect("default tv library");
        std::fs::create_dir_all(&library.root_paths[0]).unwrap();
        let media_id = domain::MediaId::new();
        guard
            .insert_media(&domain::Media {
                id: media_id,
                kind: domain::MediaKind::Tv,
                title: "Busy Detail Series".into(),
                year: Some(2024),
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let path = library.root_paths[0].join("S01E01.mkv");
        std::fs::write(&path, b"fixture").unwrap();
        store::test_support::publish_fixture_file(
            guard,
            &store::test_support::FixtureFile {
                media_id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        (library.id, media_id)
    };

    let app = router(state.clone());

    let held_store = state.store();
    let library_db = tmp.path().join("data/library.db");
    let (locked, acquired) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard =
            crate::writer_hold::WriterHold::acquire(held_store.clone(), store::Db::Library);
        let writer = rusqlite::Connection::open(library_db).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked.send(()).unwrap();
        // Stay held long enough that a hanging request cannot free itself by
        // waiting out this timeout the way a 60s probe worker panic would.
        let _ = released.recv_timeout(Duration::from_secs(30));
        writer.execute_batch("ROLLBACK").unwrap();
    });
    acquired.recv_timeout(Duration::from_secs(2)).unwrap();

    let detail = format!("/api/v1/libraries/{library_id}/items/{media_id}");
    let files = format!("/api/v1/libraries/{library_id}/items/{media_id}/files");
    let probe_status =
        format!("/api/v1/libraries/{library_id}/items/{media_id}/probe-status?season=1");

    for uri in [detail, files, probe_status] {
        let app = app.clone();
        let token = token.clone();
        let request_uri = uri.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(async {
                let response = app
                    .oneshot(request("GET", &request_uri, Some(&token)))
                    .await
                    .unwrap();
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                (status, body)
            });
            let _ = tx.send(result);
        });
        let (status, body) = rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_else(|_| {
                panic!(
                    "{uri} timed out behind the writer lock after {:?}",
                    started.elapsed()
                )
            });
        let waited = started.elapsed();
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri}: {}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            waited < Duration::from_millis(500),
            "{uri} waited {waited:?} behind the writer lock"
        );
    }
    release.send(()).unwrap();
    holder.join().unwrap();
}
