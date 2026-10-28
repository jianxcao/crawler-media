use std::collections::BTreeMap;
use std::sync::Arc;

use marker::adaptive::TemplateReference;
use parking_lot::Mutex;
use store::playback::MediaMergeRequest;

use super::*;

fn media(store: &store::Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&domain::Media {
            id,
            kind: domain::MediaKind::Tv,
            title: "Season model".into(),
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
    id
}

fn model(file: &domain::LibraryFile) -> TemplateModel {
    TemplateModel {
        model_id: "pipeline-model".into(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![TemplateReference {
            sample_id: "pipeline-sample".into(),
            ledger_id: file.file_id.to_string(),
            episode: 1,
            source_version: file.version.source_version.clone(),
            match_interval_ms: (0, 20_000),
            match_from_end_ms: None,
        }],
        expected_duration_ms: 20_000,
        is_stable: false,
    }
}

fn publish(manager: &ProbeManager, media_id: MediaId, model: &TemplateModel) {
    let policy = SamplingPolicy::default();
    let ctx = adaptive_capture_context(
        manager,
        Arc::new(crate::fingerprint_job::adaptive::NoopGate),
    );
    persist_season_model(
        &SeasonPipeline {
            mgr: manager,
            job_id: "pipeline-job",
            media_id,
            season: 1,
            profile_key: "profile",
            policy: &policy,
            ctx: &ctx,
            cost_summary: SourceCostSummary::default(),
        },
        model,
    );
}

#[test]
fn pipeline_publication_drops_transferred_media_without_redirecting() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(store::Store::open(dir.path()).unwrap()));
    let manager = ProbeManager::new(store.clone());
    let guard = store.lock();
    let loser = media(&guard);
    let survivor = media(&guard);
    let library = guard
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap();
    let file = store::test_support::publish_fixture_file(
        &guard,
        &store::test_support::FixtureFile {
            media_id: loser,
            path: library.root_paths[0]
                .join("episode.mkv")
                .display()
                .to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let captured_model = model(&file);
    let operation = guard
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: MediaId::new().to_string(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: BTreeMap::new(),
            },
            1000,
        )
        .unwrap();
    guard.advance_media_merge(&operation, 1100).unwrap();
    let current = guard.file_coverage_by_id(file.file_id).unwrap().unwrap();
    assert_eq!(current.file.media_id, survivor);
    assert_eq!(
        current.file.version.source_version,
        file.version.source_version
    );
    drop(guard);
    publish(&manager, loser, &captured_model);
    let guard = store.lock();
    assert!(
        guard
            .list_fingerprint_models(&loser.to_string(), 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        guard
            .list_fingerprint_models(&survivor.to_string(), 1)
            .unwrap()
            .is_empty()
    );
    drop(guard);
    // A newly computed pipeline for the current identity remains publishable.
    publish(&manager, survivor, &captured_model);
    assert_eq!(
        store
            .lock()
            .list_fingerprint_models(&survivor.to_string(), 1)
            .unwrap()
            .len(),
        1
    );
}
