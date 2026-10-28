//! Media operations and background artwork must keep a single Tokio worker free.

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

#[path = "executor_media_artwork/background.rs"]
mod background;
#[path = "executor_media_artwork/operations.rs"]
mod operations;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

// No runtime exists during construction, so unrelated background drivers stay off.
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
    let user = domain::User {
        id: domain::UserId::new(),
        login: "artwork-admin".into(),
        enabled: true,
        role: UserRole::Admin,
    };
    let token = "artwork-admin-session".to_string();
    {
        let handle = state.store();
        let store = handle.lock();
        store
            .insert_user_with_password(&user, "admin-pass")
            .unwrap();
        store.set_user_token(user.id, &token).unwrap();
    }
    (state, token)
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

fn assert_contended_operation_stays_responsive(method: &str, uri: &str, body: Value) {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let app = router(state.clone());
    let runtime = runtime();
    let store = state.store();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = store.lock();
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    locked_rx.recv().unwrap();
    let (response, elapsed) = runtime.block_on(async {
        let started = Instant::now();
        tokio::join!(app.oneshot(request(method, uri, &token, body)), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            started.elapsed()
        })
    });
    holder.join().unwrap();
    assert_eq!(response.unwrap().status(), StatusCode::NOT_FOUND);
    assert!(
        elapsed < Duration::from_millis(200),
        "{uri} parked the executor for {elapsed:?}"
    );
}

#[test]
fn merge_status_does_not_park_executor_on_store() {
    assert_contended_operation_stays_responsive(
        "GET",
        "/api/v1/media/operations/00000000-0000-0000-0000-00000000dead",
        Value::Null,
    );
}

#[test]
fn merge_write_does_not_park_executor_on_store() {
    assert_contended_operation_stays_responsive(
        "POST",
        "/api/v1/media/merges",
        json!({
            "request_id": MediaId::new().to_string(),
            "survivor": MediaId::new().to_string(),
            "loser": MediaId::new().to_string(),
            "survivor_revision": 0,
            "loser_revision": 0,
        }),
    );
}

// Authentication uses its own AppReader; contend the handler Store instead.
// The independent holder releases even if Tokio is parked.
#[test]
fn library_detail_and_file_page_do_not_park_executor() {
    for suffix in ["", "/files?limit=1"] {
        let tmp = tempfile::tempdir().unwrap();
        let (state, token) = state_with_admin(tmp.path());
        let library_id = state.store().lock().list_libraries().unwrap()[0].id.clone();
        let app = router(state.clone());
        let runtime = runtime();
        let store = state.store();
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = store.lock();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        rx.recv().unwrap();
        // Session authentication reads app.db without taking the Store mutex.
        let uri = format!(
            "/api/v1/libraries/{library_id}/items/{}{suffix}",
            MediaId::new()
        );
        let elapsed = runtime.block_on(async {
            let started = Instant::now();
            let (response, elapsed) = tokio::join!(
                app.oneshot(request("GET", &uri, &token, Value::Null)),
                async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    started.elapsed()
                }
            );
            let expected = if suffix.is_empty() {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            };
            assert_eq!(response.unwrap().status(), expected);
            elapsed
        });
        holder.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(200),
            "{uri} blocked for {elapsed:?}"
        );
    }
}

struct SlowArtwork {
    entered: Arc<tokio::sync::Notify>,
}

impl api::catalog::Catalog for SlowArtwork {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn image_candidates(
        &self,
        _: MediaKind,
        _: &str,
    ) -> Result<api::catalog::ArtworkCandidates, String> {
        self.entered.notify_one();
        std::thread::sleep(Duration::from_millis(500));
        Ok(api::catalog::ArtworkCandidates::default())
    }
}

fn publish_in_progress_movie(state: &ApiState, token: &str, name: &str) -> std::path::PathBuf {
    let handle = state.store();
    let store = handle.lock();
    let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
    let dir = library.root_paths[0].join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join(format!("{name}.mkv"));
    std::fs::write(&video, b"fixture video").unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: name.into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some(name.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id,
            path: video.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    let user_id = store.user_by_token(token).unwrap().unwrap().id;
    store
        .upsert_unit(
            user_id,
            media_id,
            -1,
            -1,
            1000,
            None,
            None,
            Some(100_000),
            None,
            None,
            false,
            1000,
        )
        .unwrap();
    dir.join("fanart.jpg")
}

#[test]
fn up_next_missing_artwork_does_not_park_executor() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let entered = Arc::new(tokio::sync::Notify::new());
    let state = state.with_catalog(Arc::new(SlowArtwork {
        entered: entered.clone(),
    }));
    publish_in_progress_movie(&state, &token, "slow-artwork");
    let app = router(state);
    let runtime = runtime();
    let (response, elapsed) = runtime.block_on(async {
        let started = Instant::now();
        tokio::join!(
            app.oneshot(request(
                "GET",
                "/api/v1/playback/up-next",
                &token,
                Value::Null
            )),
            async {
                tokio::time::timeout(Duration::from_secs(2), entered.notified())
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_millis(200),
        "background Catalog call parked the executor for {elapsed:?}"
    );
}
