use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, MediaId, MediaKind, QualitySource, UnitKey,
};
use parking_lot::Mutex;
use api::probe_manager::progress::MetadataPriorityGate;
use api::probe_manager::{ProbeManager, ProbeUnit};
use api::fingerprint_job::CaptureGate;
use api::Store;

/// Whole-work coverage fact, hand-built: this test only queues the unit and never
/// probes it, so the file fact cannot come from `publish_fixture_file`.
fn coverage() -> FileCoverage {
    FileCoverage {
        file: LibraryFile {
            file_id: LedgerId::new(),
            library_id: LibraryId::new(),
            root_id: "fixture-root".into(),
            media_id: MediaId::new(),
            kind: MediaKind::Movie,
            path: PathBuf::from("Test.mkv"),
            version: FileVersion {
                revision: 1,
                source_version: "fixture".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: None,
            stat_known: false,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units: vec![UnitKey::WHOLE],
    }
}

#[tokio::test]
async fn new_metadata_waits_only_for_current_sample() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let mgr = Arc::new(ProbeManager::new(store.clone()));

    let gate = MetadataPriorityGate::new(mgr.clone());

    // Initially with 0 pending metadata, gate wait returns immediately
    let res = tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(res.is_ok(), "Gate should not wait when no metadata is pending");

    let unit = ProbeUnit {
        row: coverage(),
        kind: MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    // Queue metadata
    mgr.enqueue(unit);

    // Gate should wait while metadata is pending
    let wait_res = tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(wait_res.is_err(), "Gate must wait when metadata is pending in queue");

    // Finish metadata
    mgr.metadata_stage_finished();

    // Now gate returns
    let wait_res2 = tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(wait_res2.is_ok(), "Gate should unblock after metadata finishes");
}

#[tokio::test]
async fn metadata_overrides_endpoint_and_priority_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2023),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("100".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value("Manual Title".into());
    fields.overview = domain::FieldValue::Empty; // 手工Empty

    let rev = store
        .set_metadata_overrides(media.id, domain::EntityScope::Work, "zh-CN", &fields, 0)
        .expect("set_metadata_overrides");
    assert!(rev >= 1);

    // Conflict test: wrong expected_revision should return conflict error
    let conflict = store.set_metadata_overrides(media.id, domain::EntityScope::Work, "zh-CN", &fields, 0);
    assert!(conflict.is_err(), "Expected 409 conflict when revision does not match");
}

