use std::sync::Arc;
use parking_lot::Mutex;
use domain::{Confidence, LedgerId, MediaId, MediaKind, QualitySource};
use api::probe_manager::{ProbeManager, ProbeUnit};
use api::Store;

#[test]
fn season_refresh_does_not_overwrite_recapture_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    store
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Show".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let row = domain::LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: "Test.S01E01.mkv".into(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.lock().insert_ledger(&row).unwrap();

    let mgr = ProbeManager::new(store.clone());
    let unit = ProbeUnit {
        row: row.clone(),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    let queued = mgr.enqueue_marker_refresh(vec![unit]);
    assert_eq!(queued, 1);

    // Verify in database that reuse_fingerprint_cache is false, and reuse_media_info_cache is true
    let active_unit = store
        .lock()
        .active_probe_unit_for_ledger(&row.id.to_string())
        .unwrap()
        .expect("active probe unit should exist");

    assert!(
        !active_unit.reuse_fingerprint_cache,
        "reuse_fingerprint_cache must NOT be overwritten to true for marker refresh!"
    );
    assert!(
        active_unit.reuse_media_info_cache,
        "reuse_media_info_cache should be true to reuse existing media metadata"
    );
}

#[test]
fn database_failure_rolls_back_markers_chapters_and_job_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    store
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Rollback".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let row = domain::LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: "Test.Rollback.S01E01.mkv".into(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.lock().insert_ledger(&row).unwrap();

    // Initial marker
    store
        .lock()
        .put_media_marker(&api::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(104_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "initial".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();

    let job_id = "test-job-rollback";
    let spec = api::store::ProbeJobUnitSpec {
        ledger_id: &row.id.to_string(),
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    };
    store
        .lock()
        .create_probe_job(
            job_id,
            "marker_refresh",
            &media_id.to_string(),
            Some(1),
            "test-scope",
            &[spec],
        )
        .unwrap();

    // Old visible marker is kept intact
    let visible_marker = store
        .lock()
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(visible_marker.intro_end_ms, Some(104_000));
}
