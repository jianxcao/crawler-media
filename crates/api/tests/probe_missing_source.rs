use std::sync::Arc;

use api::probe_manager::{
    ProbeManager, ProbeUnit,
    policy::{ProbeRequestOrigin, ProbeRequestResult},
};
use domain::{
    Confidence, FileCoverage, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::{FilePresence, LedgerPresenceMode, LedgerPresenceObservation, ScannedLedger, Store};

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<Store>,
    /// The scanned file after presence verification marked its source missing.
    row: FileCoverage,
}

/// One scanned file whose bytes were removed and whose presence is recorded as missing.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
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
        title: "Missing fixture".into(),
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
    let path = root.join("movie.strm");
    std::fs::write(&path, b"http://fixture.invalid/movie").unwrap();
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id: LedgerId::new(),
                media_id: media.id,
                path: path.to_string_lossy().into_owned(),
                units: vec![UnitKey::WHOLE],
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version: "fixture-v1".into(),
                source_digest: None,
                size_bytes: None,
                created_ns: None,
                modified_ns: None,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            100,
        )
        .unwrap();
    let row = store
        .file_coverage_by_path(&path.to_string_lossy())
        .unwrap()
        .expect("scanned file row");
    store
        .apply_ledger_presence(
            &[LedgerPresenceObservation {
                file: row.clone(),
                path_key: row.path_key.clone(),
                missing_at: None,
                presence: FilePresence::Missing,
            }],
            LedgerPresenceMode::Verify,
            100,
        )
        .unwrap();
    Fixture {
        _temp: temp,
        store: Arc::new(store),
        row,
    }
}

#[test]
fn missing_source_is_not_admitted_even_when_file_reappears_before_reconciliation() {
    let fixture = fixture();
    let store = &fixture.store;
    let row = &fixture.row;
    let manager = ProbeManager::new(store.clone()).unwrap();
    for origin in [
        ProbeRequestOrigin::Detail,
        ProbeRequestOrigin::Filesystem,
        ProbeRequestOrigin::Playback,
        ProbeRequestOrigin::BackgroundRetry,
        ProbeRequestOrigin::ManualRetry,
    ] {
        assert_eq!(
            manager.request_probe(row, origin).unwrap(),
            ProbeRequestResult::Cancelled
        );
    }
    let file_id = row.file.file_id.to_string();
    assert!(
        store
            .as_ref()
            .active_probe_unit_for_ledger(&file_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(manager.queued_ledger_count(), 0);
    assert!(
        store
            .as_ref()
            .clear_ledger_missing(&row.file.path.to_string_lossy())
            .unwrap()
    );
    assert!(matches!(
        manager
            .request_probe(row, ProbeRequestOrigin::Filesystem)
            .unwrap(),
        ProbeRequestResult::Queued { .. }
    ));
}

#[test]
fn recovery_retires_missing_source_queued_after_presence_snapshot() {
    let fixture = fixture();
    let store = &fixture.store;
    let file_id = fixture.row.file.file_id.to_string();
    store
        .as_ref()
        .create_probe_job(
            "late",
            "media_probe",
            &fixture.row.file.media_id.to_string(),
            None,
            "late",
            &[store::ProbeJobUnitSpec {
                ledger_id: &file_id,
                kind: "movie",
                force_fingerprint: false,
                reuse_fingerprint_cache: true,
                overwrite_markers: false,
                reuse_media_info_cache: true,
            }],
        )
        .unwrap();
    let manager = ProbeManager::new(store.clone()).unwrap();
    assert_eq!(
        store
            .as_ref()
            .get_probe_job("late")
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
    assert!(!manager.is_queued(&file_id));
    assert!(
        store
            .as_ref()
            .file_coverage_by_id(fixture.row.file.file_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn missing_source_cannot_be_persisted_by_any_admission_path() {
    let fixture = fixture();
    let store = &fixture.store;
    let manager = ProbeManager::new(store.clone()).unwrap();
    let unit = ProbeUnit {
        row: fixture.row.clone(),
        kind: MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    // These entry points can race with presence after a request's assessment.
    assert!(!manager.enqueue(unit.clone()));
    assert!(!manager.enqueue_force(unit.clone()));
    assert!(
        manager
            .enqueue_direct_fingerprint(unit, Some(120_000), "fixture".into(), 90)
            .is_none()
    );
    assert!(
        store
            .as_ref()
            .active_probe_unit_for_ledger(&fixture.row.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert_eq!(manager.queued_ledger_count(), 0);
}
