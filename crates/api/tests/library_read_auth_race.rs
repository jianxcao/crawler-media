//! Router authorization is checked again after leased SQL finishes.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use std::time::Instant;

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::QueryTiming;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

#[test]
fn revoked_original_token_cannot_publish_completed_facet_read() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = Arc::new(AtomicBool::new(true));
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_library_read_progress_observer(Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    runtime.block_on(async {
        let app = router(state.clone());
        let request = Request::get(format!("/api/v1/libraries/{library}/facets"))
            .header("Authorization", "Bearer management-secret")
            .body(Body::empty())
            .unwrap();
        let reader = tokio::spawn(app.oneshot(request));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("data/app.db")).unwrap();
        let changed = conn
            .execute(
                "DELETE FROM user_tokens WHERE token=?1",
                ["management-secret"],
            )
            .unwrap();
        assert_eq!(changed, 1);
        release_tx.send(()).unwrap();
        assert_eq!(
            reader.await.unwrap().unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    });
}

fn role_flip_status(flips: usize) -> (StatusCode, usize) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let app_db = dir.path().join("data/app.db");
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = observed.clone();
    let state = ApiState::new(
        store, "management-secret".into(), ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    ).unwrap().with_library_read_progress_observer(Arc::new(move || {
        let attempt = count.fetch_add(1, Ordering::SeqCst);
        if attempt < flips {
            let conn = rusqlite::Connection::open(&app_db).unwrap();
            let role = if attempt.is_multiple_of(2) { "member" } else { "admin" };
            conn.execute(
                "UPDATE users SET role=?1 WHERE id=(SELECT user_id FROM user_tokens WHERE token='management-secret')",
                [role],
            ).unwrap();
        }
    }));
    let status = runtime.block_on(async {
        let response = router(state)
            .oneshot(
                Request::get(format!("/api/v1/libraries/{library}/facets"))
                    .header("Authorization", "Bearer management-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        response.status()
    });
    (status, observed.load(Ordering::SeqCst))
}

#[test]
fn one_role_change_retries_entire_attempt() {
    let (status, attempts) = role_flip_status(1);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(attempts, 2);
}

#[test]
fn two_role_changes_reject_without_a_third_attempt() {
    let (status, attempts) = role_flip_status(2);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(attempts, 2);
}

#[test]
fn early_library_headers_reject_role_change_after_cover_observation() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_library_header_barrier(Arc::new(move || {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }));
    runtime.block_on(async {
        let app = router(state);
        let request = Request::get(format!("/Items?ParentId={library}&Recursive=false"))
            .header("Authorization", "Bearer management-secret")
            .header("X-Emby-Device-Id", "device-1")
            .body(Body::empty()).unwrap();
        let reader = tokio::spawn(app.oneshot(request));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        }).await.unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("data/app.db")).unwrap();
        conn.execute("UPDATE users SET role='member' WHERE id=(SELECT user_id FROM user_tokens WHERE token='management-secret')", []).unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(reader.await.unwrap().unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
    });
}

#[test]
fn retry_uses_remaining_deadline_and_timed_out_lease_recovers() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let app_db = dir.path().join("data/app.db");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = attempts.clone();
    let state = ApiState::new(
        store, "management-secret".into(), ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    ).unwrap()
    .with_library_read_pool_size(std::num::NonZeroUsize::new(1).unwrap()).unwrap()
    .with_library_read_progress_observer(Arc::new(move || {
        let attempt = observed.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            let conn = rusqlite::Connection::open(&app_db).unwrap();
            conn.execute("UPDATE users SET role='member' WHERE id=(SELECT user_id FROM user_tokens WHERE token='management-secret')", []).unwrap();
        }
        if attempt < 2 { std::thread::sleep(Duration::from_millis(180)); }
    }));
    runtime.block_on(async {
        let app = router(state);
        let request = |timing: Option<QueryTiming>| {
            let mut request = Request::get(format!("/api/v1/libraries/{library}/facets"))
                .header("Authorization", "Bearer management-secret")
                .body(Body::empty())
                .unwrap();
            if let Some(timing) = timing {
                request.extensions_mut().insert(timing);
            }
            request
        };
        let started = Instant::now();
        let deadline = started + Duration::from_millis(290);
        let response = app
            .clone()
            .oneshot(request(Some(QueryTiming { started, deadline })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let mut recovered = false;
        for _ in 0..20 {
            let status = app.clone().oneshot(request(None)).await.unwrap().status();
            if status == StatusCode::OK {
                recovered = true;
                break;
            }
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(recovered, "worker must eventually return its lease");
    });
}

#[test]
fn broken_library_query_returns_500_not_empty_success() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
    conn.execute_batch("DROP TABLE library_items").unwrap();
    runtime.block_on(async {
        let response = router(state)
            .oneshot(
                Request::get(format!("/api/v1/libraries/{library}/facets"))
                    .header("Authorization", "Bearer management-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    });
}

#[test]
fn early_library_headers_reject_acl_change_after_cover_observation() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_library_header_barrier(Arc::new(move || {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }));
    runtime.block_on(async {
        let request = Request::get(format!("/Items?ParentId={library}&Recursive=false"))
            .header("Authorization", "Bearer management-secret")
            .header("X-Emby-Device-Id", "device-1")
            .body(Body::empty())
            .unwrap();
        let reader = tokio::spawn(router(state).oneshot(request));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
        conn.execute(
            "UPDATE libraries SET admin_visible=0 WHERE library_id=?1",
            [library],
        )
        .unwrap();
        release_tx.send(()).unwrap();
        assert_ne!(reader.await.unwrap().unwrap().status(), StatusCode::OK);
    });
}

fn protocol_role_flip_status(flips: usize) -> (StatusCode, usize) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app_db = dir.path().join("data/app.db");
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = observed.clone();
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        "management-secret".into(), ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    ).unwrap().with_library_read_progress_observer(Arc::new(move || {
        let attempt = count.fetch_add(1, Ordering::SeqCst);
        if attempt < flips {
            let conn = rusqlite::Connection::open(&app_db).unwrap();
            let role = if attempt.is_multiple_of(2) { "member" } else { "admin" };
            conn.execute("UPDATE users SET role=?1 WHERE id=(SELECT user_id FROM user_tokens WHERE token='management-secret')", [role]).unwrap();
        }
    }));
    let status = runtime.block_on(async {
        router(state)
            .oneshot(
                Request::get("/Items?Limit=0")
                    .header("Authorization", "Bearer management-secret")
                    .header("X-Emby-Device-Id", "device-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    });
    (status, observed.load(Ordering::SeqCst))
}

#[test]
fn protocol_one_context_change_retries_whole_resolution_and_page() {
    let (status, attempts) = protocol_role_flip_status(1);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(attempts, 2);
}

#[test]
fn protocol_two_context_changes_return_503_without_third_attempt() {
    let (status, attempts) = protocol_role_flip_status(2);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(attempts, 2);
}
