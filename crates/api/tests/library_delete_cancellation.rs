//! A canceled DELETE request must not strand a durably frozen Library.
use api::{ApiState, router};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use std::{sync::Arc, time::Duration};
use store::Store;
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: ApiState,
    store: Arc<Store>,
    app: Router,
    library_id: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(temp.path().join("data")).unwrap(),
            "secret".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        )
        .unwrap();
        let store = state.store();
        let root = temp.path().join("remove-me");
        let fallback_root = temp.path().join("keep-me");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&fallback_root).unwrap();
        let library = store
            .create_library(
                domain::MediaKind::Movie,
                "Remove me",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .create_library(
                domain::MediaKind::Movie,
                "Keep me",
                &[fallback_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let app = router(state.clone());
        Self {
            _temp: temp,
            state,
            store,
            app,
            library_id: library.id,
        }
    }
}

fn delete_request(id: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/libraries/{id}"))
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap()
}

#[test]
fn canceled_delete_still_drives_its_durable_removal() {
    let fixture = Fixture::new();
    let lease = fixture
        .state
        .library_work()
        .acquire(&fixture.store, &fixture.library_id)
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publication = std::thread::spawn(move || {
        let _ = lease.control().publish(|| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        });
        drop(lease);
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        let request = tokio::spawn(
            fixture
                .app
                .clone()
                .oneshot(delete_request(&fixture.library_id)),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture
                .store
                .library_accepts_work(&fixture.library_id)
                .unwrap()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("DELETE did not durably freeze its Library");
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        release_tx.send(()).unwrap();
        wait_for_removal(&fixture.store, &fixture.library_id).await;
    });
    publication.join().unwrap();
    let removal = fixture
        .store
        .removal_for_library(&fixture.library_id)
        .unwrap()
        .unwrap();
    assert_eq!(removal.state, "succeeded");
    assert!(
        fixture
            .store
            .get_library(&fixture.library_id)
            .unwrap()
            .is_none()
    );
    let status = runtime
        .block_on(
            fixture
                .app
                .oneshot(removal_status_request(&removal.operation_id)),
        )
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
}

async fn wait_for_removal(store: &Store, library_id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let removal = store
                .removal_for_library(library_id)
                .unwrap()
                .expect("durable removal receipt disappeared");
            if removal.state == "succeeded" {
                break;
            }
            assert_ne!(removal.state, "failed", "removal failed: {removal:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("canceled DELETE left a frozen Library without a driver");
}

fn removal_status_request(id: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!("/api/v1/library-removals/{id}"))
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap()
}
