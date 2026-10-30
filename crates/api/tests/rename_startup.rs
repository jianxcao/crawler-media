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
        let store = shared.as_ref();
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

/// A startup replay owns interrupted intents, never a request created after startup.
#[tokio::test]
async fn startup_recovery_does_not_take_over_a_live_rename_intent() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
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
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
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
        })
        .unwrap();
    let old_from = root.join("Interrupted.mkv");
    let old_to = root.join("Recovered.mkv");
    let live_from = root.join("Live.mkv");
    let live_to = root.join("Live-renamed.mkv");
    let mut files = Vec::new();
    for path in [&old_from, &live_from] {
        std::fs::write(path, b"finite rename fixture").unwrap();
        files.push(
            publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
    }
    // Sort the interrupted intent last: its completion proves the whole old
    // recovery batch has passed a live intent if startup incorrectly claims it.
    let old = store
        .begin_file_rename(files[0].file_id, &files[0].version, &old_from, &old_to, 3)
        .unwrap();
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        root,
    )
    .unwrap();
    // No await has let the spawned startup replay run yet.
    let shared = state.store();
    let live = shared
        .begin_file_rename(files[1].file_id, &files[1].version, &live_from, &live_to, 2)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while shared.load_file_rename(&old.rename_id).is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup must recover its interrupted intent");
    assert!(old_to.is_file());
    assert!(!old_from.exists());
    let pending = shared
        .load_file_rename(&live.rename_id)
        .expect("the foreground owner must retain its live intent");
    assert_eq!(pending.phase, "planned");
    assert!(
        live_from.is_file(),
        "startup must not move live request bytes"
    );
    assert!(!live_to.exists());
}
