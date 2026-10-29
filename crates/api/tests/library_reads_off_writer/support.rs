use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{LibraryFile, LibraryId, Media, MediaId, MediaKind, UserId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub state: ApiState,
    pub library: LibraryId,
    pub files: Vec<LibraryFile>,
}

impl Fixture {
    pub fn new(count: usize) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::ERROR)
            .try_init();
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Reader isolation",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        let files = (0..count)
            .map(|index| {
                let media = Media {
                    id: MediaId::new(),
                    kind: MediaKind::Movie,
                    title: format!("Reader item {index:03}"),
                    year: Some(2024),
                    original_title: None,
                    aliases: Vec::new(),
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                };
                store.insert_media(&media).unwrap();
                let path = root.join(format!("{index:03}.mkv"));
                std::fs::write(&path, b"finite local reader fixture").unwrap();
                store::test_support::publish_fixture_file(
                    &store,
                    &store::test_support::FixtureFile {
                        media_id: media.id,
                        path: path.display().to_string(),
                        ..Default::default()
                    },
                )
                .unwrap()
            })
            .collect();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        Self {
            _dir: dir,
            state,
            library,
            files,
        }
    }

    pub fn user(&self) -> UserId {
        self.state
            .store()
            .lock()
            .user_by_token(TOKEN)
            .unwrap()
            .unwrap()
            .id
    }

    pub fn items_uri(&self) -> String {
        format!(
            "/Users/{}/Items?ParentId={}&Recursive=true&Limit=50",
            self.user(),
            self.library
        )
    }
}

/// Own the writer on an OS thread, releasing it on every assertion/drop path.
/// The three-second bound prevents a failed test from stranding the thread.
pub struct HeldWriter {
    release: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HeldWriter {
    pub fn acquire(state: &ApiState) -> Self {
        let writer = state.store();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = writer.lock();
            acquired_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        let held = Self {
            release: Some(release_tx),
            worker: Some(worker),
        };
        acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        held
    }
}

impl Drop for HeldWriter {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

pub async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    get_json_with_token(app, uri, TOKEN).await
}

pub async fn get_json_with_token(app: axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::get(uri)
                .header("Authorization", format!("Bearer {token}"))
                .header("X-Emby-Device-Id", "task3-reader")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected response: {}",
        String::from_utf8_lossy(&bytes)
    );
    (
        status,
        serde_json::from_slice(&bytes).expect("successful response must be JSON"),
    )
}

pub fn item_ids(body: &Value) -> BTreeSet<String> {
    body["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_owned())
        .collect()
}
