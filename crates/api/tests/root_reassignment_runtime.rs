use api::{
    management::ApiState,
    storage_operations::{ReconfigurationRequest, begin_library_reconfiguration},
};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use std::sync::Arc;
use store::test_support::{FixtureFile, publish_fixture_file};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("no network".into()))
    }
}

#[tokio::test]
async fn root_reassignment_resumes_after_locale_without_manual_drain_or_restart() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let parent = temp.path().join("parent");
    let nested = parent.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let shared = state.store();
    let source = shared
        .lock()
        .create_library(
            MediaKind::Movie,
            "Parent",
            &[parent.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "One".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    shared.lock().insert_media(&media).unwrap();
    let path = nested.join("One.mkv");
    std::fs::write(&path, b"video").unwrap();
    let file = publish_fixture_file(
        &shared.lock(),
        &FixtureFile {
            media_id: media.id,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    begin_library_reconfiguration(
        &state,
        &source.id,
        ReconfigurationRequest {
            root_paths: None,
            display_locale: Some("en-US".into()),
        },
    )
    .unwrap();
    let target = shared
        .lock()
        .create_library(
            MediaKind::Movie,
            "Nested",
            &[nested.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let started = shared
        .lock()
        .root_reassignments_for_library(&target.id)
        .unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].state, "blocked");
    let operation_id = started[0].operation_id.clone();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let row = shared
                .lock()
                .file_coverage_by_id(file.file_id)
                .unwrap()
                .unwrap();
            if row.file.library_id.to_string() == target.id {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("root work must resume when the source is ready");
    let finished = shared
        .lock()
        .root_reassignments_for_library(&target.id)
        .unwrap();
    assert_eq!(finished[0].operation_id, operation_id);
    assert_eq!(finished[0].state, "succeeded");
    assert!(finished[0].revision > started[0].revision);
}

#[tokio::test]
async fn frozen_root_does_not_starve_an_independent_ready_root() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let shared = state.store();
    let (blocked, file_a, dir_a) = source(&shared.lock(), temp.path(), "A");
    let (_, file_b, dir_b) = source(&shared.lock(), temp.path(), "B");
    let revision = shared.lock().library_config_revision(&blocked.id).unwrap();
    shared
        .lock()
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: "hold-a".into(),
                library_id: blocked.id.parse().unwrap(),
                expected_revision: revision,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            1,
        )
        .unwrap();
    let a = shared
        .lock()
        .create_library(
            MediaKind::Movie,
            "Nested A",
            &[dir_a.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let b = shared
        .lock()
        .create_library(
            MediaKind::Movie,
            "Nested B",
            &[dir_b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if shared
                .lock()
                .file_coverage_by_id(file_b.file_id)
                .unwrap()
                .unwrap()
                .file
                .library_id
                .to_string()
                == b.id
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        shared
            .lock()
            .file_coverage_by_id(file_a.file_id)
            .unwrap()
            .unwrap()
            .file
            .library_id
            .to_string(),
        blocked.id
    );
    assert_eq!(
        shared.lock().root_reassignments_for_library(&a.id).unwrap()[0].state,
        "blocked"
    );
}

fn source(
    store: &store::Store,
    base: &std::path::Path,
    name: &str,
) -> (store::Library, domain::LibraryFile, std::path::PathBuf) {
    let root = base.join(name);
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let owner = store
        .create_library(
            MediaKind::Movie,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: name.into(),
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
    let path = nested.join("One.mkv");
    std::fs::write(&path, b"video").unwrap();
    let file = publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    (owner, file, nested)
}

#[tokio::test]
async fn idle_root_driver_releases_its_instance_budget_on_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let budget = Arc::downgrade(&state.fs_work);
    tokio::task::yield_now().await;
    drop(state);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while budget.upgrade().is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle root driver must not keep the instance budget alive");
}

#[tokio::test]
async fn formal_root_addition_queues_cross_library_ownership_until_ready() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let shared = state.store();
    let (_, file, nested) = source(&shared.lock(), temp.path(), "Source");
    let root = temp.path().join("Target");
    std::fs::create_dir_all(&root).unwrap();
    let target = shared
        .lock()
        .create_library(
            MediaKind::Movie,
            "Target",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    begin_library_reconfiguration(
        &state,
        &target.id,
        ReconfigurationRequest {
            root_paths: Some(vec![
                root.display().to_string(),
                nested.display().to_string(),
            ]),
            display_locale: None,
        },
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if shared
                .lock()
                .file_coverage_by_id(file.file_id)
                .unwrap()
                .unwrap()
                .file
                .library_id
                .to_string()
                == target.id
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("new roots from the formal operation must drive cross-Library ownership");
    assert!(
        shared
            .lock()
            .root_reassignments_for_library(&target.id)
            .unwrap()
            .iter()
            .all(|status| status.state == "succeeded")
    );
}
