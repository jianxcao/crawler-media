use std::sync::Arc;

use domain::{EntityScope, Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, publish_fixture_file};

use super::{ExportPreparation, begin_sidecar_export, finish_sidecar_export};

struct FixedClock(i64);
impl crate::playback_events::clock::PlaybackClock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

struct EmptyFetcher;
impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unexpected request".into()))
    }
}

#[test]
fn recent_sidecar_export_survives_retention_until_its_millisecond_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = store::Store::open(temp.path().join("data")).unwrap();
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
    let video = root.join("Movie.mkv");
    std::fs::write(&video, b"movie").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: video.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    let now_ms = 1_800_000_000_000;
    let state = crate::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        root,
    )
    .unwrap()
    .with_playback_clock(Arc::new(FixedClock(now_ms)));
    let ExportPreparation::Ledgered(request) =
        begin_sidecar_export(&state, &media, &video, EntityScope::Work).unwrap()
    else {
        panic!("fixture must be ledgered");
    };
    let operation_id = request.operation_id.clone();
    finish_sidecar_export(&state, request.clone());
    let observation = store::metadata::export_driver::observe_nfo_sidecar(
        &video.with_extension("nfo"),
        EntityScope::Work,
    )
    .unwrap();
    let baseline_before = state
        .store()
        .nfo_local_edits_observed(&request, &observation)
        .unwrap();
    assert!(baseline_before.is_some());
    let handle = state.store();
    let store = handle.as_ref();
    assert!(
        store
            .nfo_export_registration(&operation_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .prune_terminal_operations(now_ms - 30 * 86_400_000, 100, false)
            .unwrap()
            .deleted,
        0
    );
    assert!(
        store
            .nfo_export_registration(&operation_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .prune_terminal_operations(now_ms + 1, 100, false)
            .unwrap()
            .deleted,
        1
    );
    assert_eq!(
        store
            .nfo_local_edits_observed(&request, &observation)
            .unwrap(),
        baseline_before
    );
}
