//! Startup resumes both rename crash positions before ordinary scan resumption.
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use store::test_support::{FixtureFile, publish_fixture_file};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

#[tokio::test]
async fn startup_resumes_planned_and_disk_staged_rename_intents() {
    for crash_after_disk in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let from = root.join("Original.mkv");
        let to = root.join("Renamed.mkv");
        std::fs::write(&from, b"video").unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
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
            year: Some(2024),
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: from.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let request = store
            .begin_file_rename(file.file_id, &file.version, &from, &to, 1)
            .unwrap();
        if crash_after_disk {
            store::library_write::execute_rename_disk(&request).unwrap();
        }
        drop(store);
        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            root,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let done = state
                    .store()
                    .lock()
                    .list_due_file_rename_ids(2)
                    .unwrap()
                    .is_empty();
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("startup must finish the durable intent");
        let shared = state.store();
        let store = shared.lock();
        let after = store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file;
        assert_eq!(after.path, to);
        assert_eq!(after.version, file.version);
        assert!(to.is_file());
        assert!(!from.exists());
        assert!(store.load_file_rename(&request.rename_id).is_err());
    }
}
