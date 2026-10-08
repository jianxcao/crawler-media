use std::sync::Arc;
use std::time::Duration;
use domain::{Confidence, LedgerId, MediaId, MediaKind, QualitySource};
use parking_lot::Mutex;
use api::probe_manager::progress::MetadataPriorityGate;
use api::probe_manager::{ProbeManager, ProbeUnit};
use api::fingerprint_job::CaptureGate;
use api::Store;

#[tokio::test]
async fn new_metadata_waits_only_for_current_sample() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let mgr = Arc::new(ProbeManager::new(store.clone()));

    let gate = MetadataPriorityGate::new(mgr.clone());

    // Initially with 0 pending metadata, gate wait returns immediately
    let res = tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(res.is_ok(), "Gate should not wait when no metadata is pending");

    let row = domain::LedgerRow {
        id: LedgerId::new(),
        media_id: MediaId::new(),
        path: "Test.mkv".into(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    let unit = ProbeUnit {
        row,
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
