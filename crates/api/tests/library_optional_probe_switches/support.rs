use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{LibraryFile, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline optional probe fixture".into()))
    }
}

pub struct Job {
    pub id: String,
    pub library: String,
    pub kind: &'static str,
    pub file: LibraryFile,
    pub initial: &'static str,
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub state: ApiState,
    pub target: String,
    pub jobs: Vec<Job>,
}

impl Fixture {
    // Construct before entering a runtime; dispatch stays idle and durable states
    // are controlled through public Store transitions, including running units.
    pub fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let mut jobs = Vec::new();
        let mut libraries = Vec::new();
        {
            let handle = state.store();
            let store = handle.lock();
            for name in ["target", "sibling"] {
                let root = dir.path().join(name);
                std::fs::create_dir_all(&root).unwrap();
                let library = store
                    .create_library(
                        MediaKind::Tv,
                        name,
                        &[root.to_str().unwrap()],
                        "everyone",
                        true,
                        &[],
                    )
                    .unwrap();
                store
                    .set_library_intro_settings(&library.id, true, true)
                    .unwrap();
                let media = Media {
                    id: MediaId::new(),
                    kind: MediaKind::Tv,
                    title: name.into(),
                    year: None,
                    original_title: None,
                    aliases: Vec::new(),
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                };
                store.insert_media(&media).unwrap();
                for (episode, (kind, status)) in [
                    ("fingerprint_probe", "queued"),
                    ("fingerprint_probe", "running"),
                    ("marker_refresh", "queued"),
                    ("marker_refresh", "running"),
                    ("media_probe", "queued"),
                    ("media_probe", "running"),
                ]
                .into_iter()
                .enumerate()
                {
                    let path = root.join(format!("S01E{:02}.mkv", episode + 1));
                    std::fs::write(&path, b"finite local probe source").unwrap();
                    let file = store::test_support::publish_fixture_file(
                        &store,
                        &store::test_support::FixtureFile {
                            media_id: media.id,
                            path: path.display().to_string(),
                            season: Some(1),
                            episode: Some(episode as u32 + 1),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    let id = uuid::Uuid::new_v4().to_string();
                    create_job(&store, &id, kind, &file);
                    if status == "running" {
                        assert!(
                            store
                                .start_probe_unit(&id, &file.file_id.to_string())
                                .unwrap()
                        );
                    }
                    jobs.push(Job {
                        id,
                        library: library.id.clone(),
                        kind,
                        file,
                        initial: status,
                    });
                }
                libraries.push(library.id);
            }
        }
        Self {
            _dir: dir,
            state,
            target: libraries[0].clone(),
            jobs,
        }
    }

    pub async fn patch(&self, field: &str, enabled: bool) {
        let response = api::router(self.state.clone())
            .oneshot(
                Request::patch(format!("/api/v1/libraries/{}", self.target))
                    .header("Authorization", "Bearer management-secret")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({field: enabled}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["data"][field], enabled, "{body}");
    }
}

pub fn create_job(store: &Store, id: &str, kind: &str, file: &LibraryFile) {
    let ledger = file.file_id.to_string();
    assert!(
        store
            .create_probe_job(
                id,
                kind,
                &file.media_id.to_string(),
                Some(1),
                &format!("{kind}:{id}"),
                &[store::ProbeJobUnitSpec {
                    ledger_id: &ledger,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: kind == "marker_refresh",
                    reuse_media_info_cache: true,
                }]
            )
            .unwrap()
    );
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
