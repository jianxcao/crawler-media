//! Library roster and every Collection route keep synchronous work off Tokio.

use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "executor_remaining_store/budgets.rs"]
mod budgets;
#[path = "executor_remaining_store/observations.rs"]
mod observations;
#[path = "executor_remaining_store/rules.rs"]
mod rules;
#[path = "executor_remaining_store/settings.rs"]
mod settings;
#[path = "executor_remaining_store/subscriptions.rs"]
mod subscriptions;
#[path = "executor_remaining_store/writes.rs"]
mod writes;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    state: ApiState,
    token: String,
    user: domain::UserId,
    collection: String,
    media: MediaId,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "executor-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let user = domain::User {
            id: domain::UserId::new(),
            login: "remaining-admin".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        let token = "remaining-admin-token".to_string();
        let media = MediaId::new();
        let collection = {
            let handle = state.store();
            let store = handle.lock();
            store
                .insert_user_with_password(&user, "admin-pass")
                .unwrap();
            store.set_user_token(user.id, &token).unwrap();
            store
                .insert_media(&Media {
                    id: media,
                    kind: MediaKind::Movie,
                    title: "Collection fixture".into(),
                    year: None,
                    original_title: None,
                    aliases: vec![],
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
            let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
            std::fs::create_dir_all(&library.root_paths[0]).unwrap();
            let path = library.root_paths[0].join("fixture.mkv");
            std::fs::write(&path, b"fixture").unwrap();
            std::fs::write(library.root_paths[0].join("poster.jpg"), b"poster").unwrap();
            store::test_support::publish_fixture_file(
                &store,
                &store::test_support::FixtureFile {
                    media_id: media,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            let collection = store.create_collection(user.id, "Original").unwrap();
            store
                .add_collection_item(&collection, &media.to_string())
                .unwrap();
            collection
        };
        Self {
            _tmp: tmp,
            state,
            token,
            user: user.id,
            collection,
            media,
        }
    }

    fn uri(&self, suffix: &str) -> String {
        format!("/api/v1/collections/{}{}", self.collection, suffix)
    }
}

fn request(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn json_response(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()
}

fn hold_store(state: &ApiState, duration: Duration) -> std::thread::JoinHandle<()> {
    let store = state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = store.lock();
        tx.send(()).unwrap();
        std::thread::sleep(duration);
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    holder
}

#[test]
fn remaining_roster_and_collection_routes_keep_executor_responsive() {
    for route in 0..10 {
        let f = Fixture::new();
        let (method, uri, body) = match route {
            0 => ("GET", "/api/v1/libraries".into(), Value::Null),
            1 => ("GET", "/api/v1/collections".into(), Value::Null),
            2 => ("GET", f.uri("/items"), Value::Null),
            3 => ("GET", f.uri("/gallery"), Value::Null),
            4 => (
                "POST",
                "/api/v1/collections".into(),
                json!({"name":"Created"}),
            ),
            5 => ("PATCH", f.uri(""), json!({"name":"Renamed"})),
            6 => ("POST", f.uri("/items"), json!({"media_item_id": f.media})),
            7 => ("DELETE", f.uri(&format!("/items/{}", f.media)), Value::Null),
            8 => ("PUT", f.uri("/order"), json!({"media_item_ids":[f.media]})),
            _ => ("DELETE", f.uri(""), Value::Null),
        };
        let app = router(f.state.clone());
        let holder = hold_store(&f.state, Duration::from_millis(500));
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, &uri, &f.token, body)), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            })
        });
        holder.join().unwrap();
        assert_eq!(response.unwrap().status(), StatusCode::OK, "{method} {uri}");
        assert!(
            elapsed < Duration::from_millis(200),
            "{method} {uri} blocked timer: {elapsed:?}"
        );
    }
}

#[path = "executor_remaining_store/projections.rs"]
mod projections;

#[path = "executor_remaining_store/extras.rs"]
mod extras;
