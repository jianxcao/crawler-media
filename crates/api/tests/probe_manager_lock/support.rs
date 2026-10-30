use std::sync::Arc;

use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{LibraryFile, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline probe lock fixture".into()))
    }
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub state: ApiState,
    file: LibraryFile,
    session: String,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "probe-lock-test-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Movie.mkv");
        std::fs::write(&path, b"finite local fixture").unwrap();
        let (file, session) = {
            let handle = state.store();
            let store = handle.lock();
            store
                .create_library(
                    MediaKind::Movie,
                    "Movies",
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            let media = Media {
                id: MediaId::new(),
                kind: MediaKind::Movie,
                title: "Movie".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            };
            store.insert_media(&media).unwrap();
            let file = store::test_support::publish_fixture_file(
                &store,
                &store::test_support::FixtureFile {
                    media_id: media.id,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(file.duration_ms.is_none());
            let user = store
                .user_by_token("probe-lock-test-token")
                .unwrap()
                .unwrap();
            let session = store
                .issue_play_session(
                    user.id,
                    "probe-lock-device",
                    file.file_id,
                    file.media_id,
                    domain::UnitKey::WHOLE,
                    &file.version,
                    1_000,
                )
                .unwrap();
            (file, session)
        };
        Self {
            _dir: dir,
            state,
            file,
            session,
        }
    }

    pub async fn progress(self) -> (StatusCode, Value) {
        let response = api::router(self.state)
            .oneshot(
                Request::post("/api/v1/playback/progress")
                    .header("Authorization", "Bearer probe-lock-test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "device_id": "probe-lock-device", "play_session_id": self.session,
                            "event_sequence": 1, "event": "start", "position_ms": 1_000,
                            "media_item_id": self.file.media_id.to_string(),
                            "file_id": self.file.file_id.to_string()
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
}
