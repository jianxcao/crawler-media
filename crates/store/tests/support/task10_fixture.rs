#![allow(dead_code)]
use domain::{LibraryId, Media, MediaId, MediaKind, UnitKey, UserId};
use store::test_support::{FixtureFile, publish_fixture_file_units};
use store::{LedgerPresenceMode, Store};
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub user: UserId,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        Self {
            dir,
            store,
            user: UserId::new(),
        }
    }
    pub fn library(&self, name: &str) -> LibraryId {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        self.store
            .create_library(
                MediaKind::Movie,
                name,
                &[path.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap()
    }
    pub fn media(&self, n: u64) -> MediaId {
        let id = format!("00000000-0000-0000-0000-{n:012}").parse().unwrap();
        self.store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: format!("Movie {n}"),
                year: Some(2024),
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        id
    }
    pub fn publish(
        &self,
        lib: LibraryId,
        id: MediaId,
        name: &str,
        units: &[UnitKey],
    ) -> domain::LibraryFile {
        let root = self
            .store
            .get_library(&lib.to_string())
            .unwrap()
            .unwrap()
            .root_paths
            .remove(0);
        let path = root.join(name);
        std::fs::write(&path, b"fixture").unwrap();
        publish_fixture_file_units(
            &self.store,
            &FixtureFile {
                media_id: id,
                path: path.display().to_string(),
                ..Default::default()
            },
            units,
        )
        .unwrap()
    }
    pub fn presence(&self, file: &domain::LibraryFile, presence: domain::FilePresence, now: i64) {
        let mut obs = self
            .store
            .watcher_presence_page(file.path.to_str().unwrap(), None, 1)
            .unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].file.file.file_id, file.file_id);
        obs[0].presence = presence;
        self.store
            .apply_ledger_presence(&obs, LedgerPresenceMode::Verify, now)
            .unwrap();
    }
}
