//! Public Router regressions for the pre-auth query deadline.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::{QueryTiming, QueryTimingSeed};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn state(dir: &tempfile::TempDir) -> ApiState {
    ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
}

#[test]
fn writer_lock_before_auth_respects_deadline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let state = state(&dir);
    runtime.block_on(async {
        let app = router(state.clone());
        let store = state.store();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let released = Arc::new(AtomicBool::new(false));
        let released_worker = released.clone();
        let writer = std::thread::spawn(move || {
            let _guard = store.lock();
            ready_tx.send(()).unwrap();
            // This timeout only prevents a hung test from retaining the lock forever.
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
            released_worker.store(true, Ordering::SeqCst);
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let progress = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let deadline = started + Duration::from_millis(80);
        for (uri, method) in [
            (
                "/api/v1/libraries/00000000-0000-0000-0000-000000000001/items",
                "GET",
            ),
            (
                "/api/v1/libraries/00000000-0000-0000-0000-000000000001/facets",
                "GET",
            ),
            (
                "/api/v1/libraries/00000000-0000-0000-0000-000000000001/gallery",
                "GET",
            ),
            ("/Items", "GET"),
            ("/emby/Items", "GET"),
            (
                "/api/v1/libraries/00000000-0000-0000-0000-000000000001/items",
                "HEAD",
            ),
            ("/Items", "HEAD"),
            ("/emby/Items", "HEAD"),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", "Bearer management-secret")
                .header("X-Emby-Device-Id", "device-1")
                .body(Body::empty())
                .unwrap();
            let timing = QueryTiming { started, deadline };
            if method == "HEAD" {
                request.extensions_mut().insert(QueryTimingSeed(timing));
            } else {
                request.extensions_mut().insert(timing);
            }
            let progressed = progress.clone();
            tokio::spawn(async move {
                progressed.store(true, Ordering::SeqCst);
            });
            let response =
                tokio::time::timeout(Duration::from_secs(2), app.clone().oneshot(request))
                    .await
                    .expect("Router must not block current-thread runtime")
                    .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {uri}"
            );
            assert!(
                progress.load(Ordering::SeqCst),
                "unrelated Tokio task did not advance"
            );
            assert!(
                !released.load(Ordering::SeqCst),
                "writer released before query rejected"
            );
        }
        release_tx.send(()).unwrap();
        writer.join().unwrap();
    });
}

#[test]
fn self_covered_routes_distinguish_invalid_credentials_from_store_failure() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(&dir);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let app = router(state.clone());
        let paths = [
            "/api/v1/libraries/00000000-0000-0000-0000-000000000001/items",
            "/api/v1/libraries/00000000-0000-0000-0000-000000000001/facets",
            "/api/v1/libraries/00000000-0000-0000-0000-000000000001/gallery",
        ];
        for path in paths {
            let response = app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header("authorization", "Bearer invalid-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        let conn = rusqlite::Connection::open(dir.path().join("data/app.db")).unwrap();
        conn.execute_batch("DROP TABLE user_tokens").unwrap();
        for path in paths {
            let response = app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header("authorization", "Bearer management-secret")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{path}"
            );
        }
    });
}

async fn self_response(
    state: ApiState,
    library: &str,
    endpoint: &str,
    method: &str,
    token: &str,
) -> axum::response::Response {
    router(state)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/v1/libraries/{library}/{endpoint}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn self_reads_keep_missing_and_hidden_library_404_for_get_and_head() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(&dir);
    let library = state
        .store()
        .lock()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id
        .to_string();
    let member = domain::UserId::new();
    {
        let store = state.store();
        let store = store.lock();
        store
            .insert_user(&domain::User {
                id: member,
                login: "read-member".into(),
                role: domain::UserRole::Member,
                enabled: true,
            })
            .unwrap();
        store.set_user_token(member, "member-token").unwrap();
    }
    let conn = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
    conn.execute("UPDATE libraries SET admin_visible=0,access_mode='selected',member_ids_json='[]',lifecycle_state='reindexing' WHERE library_id=?1", [&library]).unwrap();
    let missing = domain::LibraryId::new().to_string();
    for endpoint in ["items", "gallery", "facets"] {
        for method in ["GET", "HEAD"] {
            for (id, token) in [
                (&missing, "management-secret"),
                (&library, "management-secret"),
                (&library, "member-token"),
            ] {
                let response = self_response(state.clone(), id, endpoint, method, token).await;
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {id}/{endpoint}"
                );
                if method == "GET" {
                    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .unwrap();
                    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body["error"]["code"], "library.missing");
                }
            }
            let response =
                self_response(state.clone(), &missing, endpoint, method, "invalid-token").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }
}

#[tokio::test]
async fn self_reads_revalidate_original_token_before_visibility_preflight() {
    for endpoint in ["items", "gallery", "facets"] {
        for method in ["GET", "HEAD"] {
            let dir = tempfile::tempdir().unwrap();
            let state = state(&dir);
            let library = state
                .store()
                .lock()
                .default_library(domain::MediaKind::Movie)
                .unwrap()
                .unwrap()
                .id
                .to_string();
            let db = dir.path().join("data/app.db");
            let fired = Arc::new(AtomicBool::new(false));
            let observed = fired.clone();
            let state = state.with_library_read_stage_barrier(Arc::new(move |stage| {
                if stage == "visibility" && !observed.swap(true, Ordering::SeqCst) {
                    let conn = rusqlite::Connection::open(&db).unwrap();
                    conn.execute(
                        "DELETE FROM user_tokens WHERE token='management-secret'",
                        [],
                    )
                    .unwrap();
                }
            }));
            let response =
                self_response(state, &library, endpoint, method, "management-secret").await;
            assert!(
                fired.load(Ordering::SeqCst),
                "must revoke after HTTP auth before scope lookup"
            );
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {endpoint}"
            );
        }
    }
}

#[tokio::test]
async fn self_read_visibility_preflight_preserves_sql_failures_and_freeze_status() {
    for failure in ["sql", "freeze"] {
        for endpoint in ["items", "gallery", "facets"] {
            for method in ["GET", "HEAD"] {
                // A structural query failure closes the pool, so each first-failure
                // status oracle needs its own instance rather than the closed pool.
                let dir = tempfile::tempdir().unwrap();
                let state = state(&dir);
                let library = state
                    .store()
                    .lock()
                    .default_library(domain::MediaKind::Movie)
                    .unwrap()
                    .unwrap()
                    .id
                    .to_string();
                let conn = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
                let expected = if failure == "sql" {
                    conn.execute_batch("DROP TABLE library_items").unwrap();
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    state
                        .store()
                        .lock()
                        .begin_library_integrity(library.parse().unwrap(), true, "freeze", 0)
                        .unwrap();
                    StatusCode::SERVICE_UNAVAILABLE
                };
                let invalid =
                    self_response(state.clone(), &library, endpoint, method, "invalid-token").await;
                assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
                let response =
                    self_response(state, &library, endpoint, method, "management-secret").await;
                assert_eq!(
                    response.status(),
                    expected,
                    "{failure}: {method} {endpoint}"
                );
                if failure == "freeze" {
                    assert_reindexing_body(response, method).await;
                }
            }
        }
    }
}

async fn assert_reindexing_body(response: axum::response::Response, method: &str) {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    if method == "HEAD" {
        assert!(bytes.is_empty());
    } else {
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "library.reindexing");
    }
}

#[tokio::test]
async fn self_reads_preserve_reindexing_started_after_visibility_preflight() {
    for endpoint in ["items", "gallery", "facets"] {
        for method in ["GET", "HEAD"] {
            let dir = tempfile::tempdir().unwrap();
            let state = state(&dir);
            let shared = state.store();
            let library = shared
                .lock()
                .default_library(domain::MediaKind::Movie)
                .unwrap()
                .unwrap()
                .id;
            let fired = Arc::new(AtomicBool::new(false));
            let observed = fired.clone();
            let library_id = library.parse().unwrap();
            let state = state.with_library_read_stage_barrier(Arc::new(move |stage| {
                if stage == "prepare" && !observed.swap(true, Ordering::SeqCst) {
                    shared
                        .lock()
                        .begin_library_integrity(library_id, true, "freeze", 0)
                        .unwrap();
                }
            }));
            let response =
                self_response(state, &library, endpoint, method, "management-secret").await;
            assert!(fired.load(Ordering::SeqCst));
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_reindexing_body(response, method).await;
        }
    }
}
