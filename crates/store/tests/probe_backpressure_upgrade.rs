//! Durable probe bookkeeping across a restart.
//!
//! There is no upgrade ladder any more: `Store::open` only initializes or
//! validates the current layout and never re-derives probe work. Work lost to a
//! full dispatch window is therefore handled at the two durable seams instead —
//! the scan-armed obligation (`arm_scan_probe_obligation`, one row per file)
//! and the acknowledged obligation (`ack_scan_probe_obligation`) — and both must
//! keep their verdict across an ordinary restart.

use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use store::{ProbeJobUnitSpec, ScannedLedger, Store};

struct Setup {
    _temp: tempfile::TempDir,
    data: std::path::PathBuf,
    file_id: LedgerId,
    media_id: MediaId,
}

impl Setup {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let file_id = LedgerId::new();
        let media_id = MediaId::new();
        let store = Store::open(&data).unwrap();
        let root = temp.path().join("movies");
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
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: "Backpressure fixture".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let path = root.join("lost.strm").to_string_lossy().into_owned();
        store
            .insert_scanned_files(&[scanned(file_id, media_id, &path)], 1_000)
            .unwrap();
        Self {
            _temp: temp,
            data,
            file_id,
            media_id,
        }
    }

    fn open(&self) -> Store {
        Store::open(&self.data).unwrap()
    }

    fn ledger_id(&self) -> String {
        self.file_id.to_string()
    }
}

/// Fixture scan row: a whole-unit file with a release observation and no NFO facts.
fn scanned(file_id: LedgerId, media_id: MediaId, path: &str) -> ScannedLedger {
    ScannedLedger {
        file_id,
        media_id,
        path: path.to_string(),
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
    }
}

fn job(store: &Store, id: &str, ledger_id: &str, media_id: MediaId) {
    assert!(
        store
            .create_probe_job(
                id,
                "media_probe",
                &media_id.to_string(),
                None,
                &format!("ledger:{ledger_id}"),
                &[ProbeJobUnitSpec {
                    ledger_id,
                    kind: "movie",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
}

#[test]
fn capacity_cancelled_work_stays_armed_and_acked_work_stays_acked() {
    let setup = Setup::new();
    let ledger_id = setup.ledger_id();
    let store = setup.open();
    job(&store, "lost", &ledger_id, setup.media_id);
    store
        .cancel_probe_unit("lost", &ledger_id, "dispatch window full")
        .unwrap();
    assert_eq!(
        store.get_probe_job("lost").unwrap().unwrap().status,
        "cancelled"
    );
    // The cancelled job keeps the backlog: the obligation the scan armed is what a
    // later dispatch round acts on, so no repair pass is needed on the next open.
    assert_eq!(store.count_probe_obligations().unwrap(), 1);
    drop(store);

    let store = setup.open();
    assert_eq!(
        store.get_probe_job("lost").unwrap().unwrap().status,
        "cancelled"
    );
    assert_eq!(store.scan_probe_obligations(100).unwrap().len(), 1);
    store.ack_scan_probe_obligation(setup.file_id).unwrap();
    drop(store);

    assert!(
        setup.open().scan_probe_obligations(100).unwrap().is_empty(),
        "ordinary restarts must not resurrect acknowledged historical work"
    );
}

#[test]
fn armed_work_is_one_durable_obligation_per_file() {
    let setup = Setup::new();
    let ledger_id = setup.ledger_id();
    let store = setup.open();
    store.ack_scan_probe_obligation(setup.file_id).unwrap();
    assert!(store.scan_probe_obligations(100).unwrap().is_empty());

    // Re-arming is idempotent: repeated observations of the same changed/restored
    // source update the durable obligation instead of piling up duplicates.
    store
        .arm_scan_probe_obligation(&ledger_id, 2_000, "scan_changed")
        .unwrap();
    store
        .arm_scan_probe_obligation(&ledger_id, 3_000, "scan_changed")
        .unwrap();
    drop(store);

    let store = setup.open();
    let obligations = store.scan_probe_obligations(100).unwrap();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].ledger_id, setup.file_id);
    assert_eq!(obligations[0].created_at_ms, 3_000);
    assert_eq!(obligations[0].reason, "scan_changed");

    store.ack_scan_probe_obligation(setup.file_id).unwrap();
    assert!(store.scan_probe_obligations(100).unwrap().is_empty());
    assert!(
        store.file_coverage_by_id(setup.file_id).unwrap().is_some(),
        "acking an obligation is not a file lifecycle event"
    );
}
