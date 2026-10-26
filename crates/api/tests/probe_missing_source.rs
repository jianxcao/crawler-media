use std::sync::Arc;

use api::probe_manager::{
    ProbeManager, ProbeUnit,
    policy::{ProbeRequestOrigin, ProbeRequestResult},
};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use parking_lot::Mutex;
use store::{FilePresence, LedgerPresenceMode, LedgerPresenceObservation, Store};

fn fixture() -> (tempfile::TempDir, Arc<Mutex<Store>>, LedgerRow) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
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
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: temp
            .path()
            .join("movie.strm")
            .to_string_lossy()
            .into_owned(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    std::fs::write(&row.path, b"http://fixture.invalid/movie").unwrap();
    store.insert_ledger(&row).unwrap();
    store
        .apply_ledger_presence(
            &[LedgerPresenceObservation {
                row: row.clone(),
                missing_at: None,
                presence: FilePresence::Missing,
            }],
            LedgerPresenceMode::Verify,
            100,
        )
        .unwrap();
    (temp, Arc::new(Mutex::new(store)), row)
}

#[test]
fn missing_source_is_not_admitted_even_when_file_reappears_before_reconciliation() {
    let (_temp, store, row) = fixture();
    let manager = ProbeManager::new(store.clone());
    for origin in [
        ProbeRequestOrigin::Detail,
        ProbeRequestOrigin::Filesystem,
        ProbeRequestOrigin::Playback,
        ProbeRequestOrigin::BackgroundRetry,
        ProbeRequestOrigin::ManualRetry,
    ] {
        assert_eq!(
            manager.request_probe(&row, origin).unwrap(),
            ProbeRequestResult::Cancelled
        );
    }
    assert!(
        store
            .lock()
            .active_probe_unit_for_ledger(&row.id.to_string())
            .unwrap()
            .is_none()
    );
    assert_eq!(manager.queued_ledger_count(), 0);
    assert!(store.lock().clear_ledger_missing(&row.path).unwrap());
    assert!(matches!(
        manager
            .request_probe(&row, ProbeRequestOrigin::Filesystem)
            .unwrap(),
        ProbeRequestResult::Queued { .. }
    ));
}

#[test]
fn recovery_retires_missing_source_queued_after_presence_snapshot() {
    let (_temp, store, row) = fixture();
    let ledger_id = row.id.to_string();
    store
        .lock()
        .create_probe_job(
            "late",
            "media_probe",
            &row.media_id.to_string(),
            None,
            "late",
            &[store::ProbeJobUnitSpec {
                ledger_id: &ledger_id,
                kind: "movie",
                force_fingerprint: false,
                reuse_fingerprint_cache: true,
                overwrite_markers: false,
                reuse_media_info_cache: true,
            }],
        )
        .unwrap();
    let manager = ProbeManager::new(store.clone());
    assert_eq!(
        store.lock().get_probe_job("late").unwrap().unwrap().status,
        "cancelled"
    );
    assert!(!manager.is_queued(&ledger_id));
    assert!(store.lock().get_ledger(&ledger_id).unwrap().is_some());
}

#[test]
fn missing_source_cannot_be_persisted_by_any_admission_path() {
    let (_temp, store, row) = fixture();
    let manager = ProbeManager::new(store.clone());
    let unit = ProbeUnit {
        row: row.clone(),
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
            .lock()
            .active_probe_unit_for_ledger(&row.id.to_string())
            .unwrap()
            .is_none()
    );
    assert_eq!(manager.queued_ledger_count(), 0);
}
