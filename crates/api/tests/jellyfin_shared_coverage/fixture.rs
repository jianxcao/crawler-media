use std::sync::Arc;

use api::media_server_provider::ApiServerProvider;
use api::{ApiState, Store};
use domain::{Media, MediaId, MediaKind, PhysicalSourceId, UnitKey, UserId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use store::test_support::{FixtureFile, publish_fixture_file_units};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub state: ApiState,
    pub user: UserId,
    pub media: MediaId,
    pub source: PhysicalSourceId,
}

impl Fixture {
    // Construct outside Tokio: no root/probe/background driver may participate in
    // the request allocation measurement. Read-only also suppresses auto-probing.
    pub fn new(episodes: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Covered episodes".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let root = store
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap()
            .root_paths[0]
            .clone();
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pack.mkv");
        std::fs::write(&path, b"offline fixture").unwrap();
        let units: Vec<_> = (1..=episodes)
            .map(|episode| UnitKey::new(1, episode))
            .collect();
        let file = publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                file_id: Some("00000000-0000-4000-8000-000000000010".parse().unwrap()),
                ..Default::default()
            },
            &units,
        )
        .unwrap();
        let source = PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        };
        let state = ApiState::new(
            store,
            "coverage-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap()
        .with_instance_writable(false);
        let user = state
            .store()
            .user_id_by_token("coverage-secret")
            .unwrap()
            .unwrap();
        state
            .store()
            .upsert_unit_for_source(
                user,
                media.id,
                UnitKey::new(1, 2),
                source.clone(),
                20_000,
                Some(false),
                Some(true),
                Some(100_000),
                None,
                None,
                false,
                1000,
            )
            .unwrap();
        Self {
            _dir: dir,
            state,
            user,
            media: media.id,
            source,
        }
    }

    pub fn provider(&self) -> ApiServerProvider {
        ApiServerProvider::new(self.state.clone())
    }
}
