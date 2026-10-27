//! Presence verdicts against durable probe work.
//!
//! A presence verdict is a file fact, not a lifecycle event: `Verify` marks
//! `missing_at` and changes nothing else, so the file row, its Media, its cold
//! details and its durable probe backlog all survive a missing source. Work is
//! retired only when the file row itself is deleted (`Prune` of an already
//! missing file), which drops that file's queued/running units and resyncs the
//! owning job.
//!
//! Re-arming a restored source is the scan's write
//! ([`Store::arm_scan_probe_obligation`]), never the presence writer's: arming
//! and retirement are one seam each, not two competing writers.

use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    ProbeJobUnitSpec, ScannedLedger, Store,
};

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    file_id: LedgerId,
    media_id: MediaId,
    path: String,
}

/// Fixture scan row: one whole-unit file with a release observation and no NFO facts.
fn scanned(file_id: LedgerId, media_id: MediaId, path: &str) -> ScannedLedger {
    ScannedLedger {
        file_id,
        media_id,
        path: path.to_string(),
        units: vec![UnitKey::new(1, 1)],
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

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("tv");
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Presence fixture".into(),
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
    let path = root.join("episode.strm").to_string_lossy().into_owned();
    let file_id = LedgerId::new();
    // The scan publishes the row and arms its durable probe obligation, so the
    // presence tests below start from the same state a real scan leaves behind.
    store
        .insert_scanned_files(&[scanned(file_id, media.id, &path)], 1_000)
        .unwrap();
    let tracks = library::Tracks {
        video: Some(library::VideoTrack::default()),
        ..Default::default()
    };
    store
        .put_file_meta_versioned(&file_id.to_string(), &tracks, None, Some(1_200_000))
        .unwrap();
    Fixture {
        _temp: temp,
        store,
        file_id,
        media_id: media.id,
        path,
    }
}

fn job(store: &Store, id: &str, kind: &str, file_id: LedgerId, media_id: MediaId) {
    let ledger_id = file_id.to_string();
    assert!(
        store
            .create_probe_job(
                id,
                kind,
                &media_id.to_string(),
                Some(1),
                id,
                &[ProbeJobUnitSpec {
                    ledger_id: &ledger_id,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
}

/// Apply one presence verdict for the fixture file, exactly as a scan or the
/// watcher does: the read shape carries the identity the observation asserts.
fn observe(
    store: &Store,
    file_id: LedgerId,
    missing_at: Option<i64>,
    presence: FilePresence,
    mode: LedgerPresenceMode,
    now: i64,
) -> LedgerPresenceResult {
    let file = store
        .file_coverage_by_id(file_id)
        .unwrap()
        .expect("recorded file coverage");
    let path_key = if file.path_key.is_empty() {
        store.recorded_path_key(&file.file.path.to_string_lossy())
    } else {
        file.path_key.clone()
    };
    store
        .apply_ledger_presence(
            &[LedgerPresenceObservation {
                file,
                path_key,
                missing_at,
                presence,
            }],
            mode,
            now,
        )
        .unwrap()
}

#[test]
fn missing_verdict_retires_no_work_and_keeps_every_fact() {
    for running in [false, true] {
        let fixture = fixture();
        let store = &fixture.store;
        let ledger_id = fixture.file_id.to_string();
        job(
            store,
            "probe",
            "media_probe",
            fixture.file_id,
            fixture.media_id,
        );
        if running {
            assert!(store.start_probe_unit("probe", &ledger_id).unwrap());
        }

        let result = observe(
            store,
            fixture.file_id,
            None,
            FilePresence::Missing,
            LedgerPresenceMode::Verify,
            100,
        );
        assert_eq!(result.missing, 1);

        // Facts the verdict must not touch: file, Media, cold details.
        let coverage = store
            .file_coverage_by_id(fixture.file_id)
            .unwrap()
            .expect("a missing verdict keeps the recorded row");
        assert_eq!(coverage.file.media_id, fixture.media_id);
        assert_eq!(coverage.units, vec![UnitKey::new(1, 1)]);
        assert!(store.get_media(fixture.media_id).unwrap().is_some());
        assert!(store.get_file_meta(&ledger_id).unwrap().is_some());

        // Work is untouched: queue state and the durable backlog survive.
        let units = store.probe_job_units("probe").unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].status, if running { "running" } else { "queued" });
        assert!(store.get_probe_job("probe").unwrap().unwrap().is_active());
        assert_eq!(store.count_probe_obligations().unwrap(), 1);
    }
}

#[test]
fn pruning_a_missing_file_cancels_queued_and_running_units() {
    for running in [false, true] {
        let fixture = fixture();
        let store = &fixture.store;
        let ledger_id = fixture.file_id.to_string();
        job(
            store,
            "probe",
            "media_probe",
            fixture.file_id,
            fixture.media_id,
        );
        if running {
            assert!(store.start_probe_unit("probe", &ledger_id).unwrap());
        }
        observe(
            store,
            fixture.file_id,
            None,
            FilePresence::Missing,
            LedgerPresenceMode::Verify,
            100,
        );

        let result = observe(
            store,
            fixture.file_id,
            Some(100),
            FilePresence::Missing,
            LedgerPresenceMode::Prune,
            200,
        );
        assert_eq!(result.deleted, 1);

        // A unit whose file row is gone can never run again: it is retired and the
        // owning job is resynced instead of being left active forever.
        assert!(store.probe_job_units("probe").unwrap().is_empty());
        assert!(
            store
                .active_probe_unit_for_ledger(&ledger_id)
                .unwrap()
                .is_none()
        );
        let probe = store.get_probe_job("probe").unwrap().unwrap();
        assert_eq!(probe.status, "cancelled");
        assert_eq!(probe.error.as_deref(), Some("file_deleted"));
        assert_eq!((probe.total, probe.completed, probe.failed), (0, 0, 0));

        // The file row, its obligation and its cold details go with the delete;
        // Media-owned facts stay.
        assert!(
            store
                .file_coverage_by_id(fixture.file_id)
                .unwrap()
                .is_none()
        );
        assert!(store.scan_probe_obligations(100).unwrap().is_empty());
        assert!(store.get_file_meta(&ledger_id).unwrap().is_none());
        assert!(store.get_media(fixture.media_id).unwrap().is_some());
    }
}

#[test]
fn restored_file_rearms_one_obligation_and_retains_prior_probe_history() {
    let fixture = fixture();
    let store = &fixture.store;
    let ledger_id = fixture.file_id.to_string();
    job(
        store,
        "probe",
        "media_probe",
        fixture.file_id,
        fixture.media_id,
    );
    // An earlier attempt already ended: its terminal history must never be rewritten
    // by a later presence verdict or by re-arming the source.
    store
        .cancel_probe_unit("probe", &ledger_id, "dispatch window full")
        .unwrap();
    observe(
        store,
        fixture.file_id,
        None,
        FilePresence::Missing,
        LedgerPresenceMode::Verify,
        100,
    );
    store.ack_scan_probe_obligation(fixture.file_id).unwrap();
    assert!(store.scan_probe_obligations(100).unwrap().is_empty());

    let restored = observe(
        store,
        fixture.file_id,
        Some(100),
        FilePresence::Present,
        LedgerPresenceMode::Verify,
        200,
    );
    assert_eq!((restored.restored, restored.missing), (1, 0));

    // The scan re-observes a restored source and arms work for it; repeated
    // observations of the same restore still leave exactly one obligation.
    store
        .arm_scan_probe_obligation(&ledger_id, 200_000, "source_restored")
        .unwrap();
    store
        .arm_scan_probe_obligation(&ledger_id, 300_000, "source_restored")
        .unwrap();
    let obligations = store.scan_probe_obligations(100).unwrap();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].ledger_id, fixture.file_id);
    assert_eq!(obligations[0].created_at_ms, 300_000);
    assert_eq!(obligations[0].reason, "source_restored");

    // Prior work history and file facts survive the missing→restored round trip.
    let probe = store.get_probe_job("probe").unwrap().unwrap();
    assert_eq!(probe.status, "cancelled");
    assert_eq!(probe.error.as_deref(), Some("dispatch window full"));
    assert_eq!(probe.completed, 1);
    assert_eq!(
        store.probe_job_units("probe").unwrap()[0].status,
        "cancelled"
    );
    store.ack_scan_probe_obligation(fixture.file_id).unwrap();
    let coverage = store.file_coverage_by_id(fixture.file_id).unwrap().unwrap();
    assert_eq!(coverage.file.media_id, fixture.media_id);
    assert!(store.get_file_meta(&ledger_id).unwrap().is_some());
    assert!(store.scan_probe_obligations(100).unwrap().is_empty());
}

#[test]
fn watcher_reappearance_clears_missing_once() {
    let fixture = fixture();
    let store = &fixture.store;
    observe(
        store,
        fixture.file_id,
        None,
        FilePresence::Missing,
        LedgerPresenceMode::Verify,
        100,
    );
    store.ack_scan_probe_obligation(fixture.file_id).unwrap();

    // A reappeared file clears the flag once; the second call has nothing left to do.
    assert!(store.clear_ledger_missing(&fixture.path).unwrap());
    assert!(!store.clear_ledger_missing(&fixture.path).unwrap());

    // The flag really was cleared: the next missing verdict is applied again.
    let missing_again = observe(
        store,
        fixture.file_id,
        None,
        FilePresence::Missing,
        LedgerPresenceMode::Verify,
        200,
    );
    assert_eq!(missing_again.missing, 1);
}

#[test]
fn unknown_or_stale_presence_does_not_cancel_probe_work() {
    let fixture = fixture();
    let store = &fixture.store;
    job(
        store,
        "probe",
        "media_probe",
        fixture.file_id,
        fixture.media_id,
    );

    // A permission/IO error is reported as Unknown and must change nothing.
    let unknown = observe(
        store,
        fixture.file_id,
        None,
        FilePresence::Unknown,
        LedgerPresenceMode::Verify,
        100,
    );
    assert_eq!(
        (unknown.skipped, unknown.missing, unknown.restored),
        (1, 0, 0)
    );

    // An observation that predates the current row state is rejected, not applied.
    let stale = observe(
        store,
        fixture.file_id,
        Some(100),
        FilePresence::Missing,
        LedgerPresenceMode::Verify,
        150,
    );
    assert_eq!((stale.skipped, stale.missing), (1, 0));

    assert_eq!(store.probe_job_units("probe").unwrap()[0].status, "queued");
    assert_eq!(store.count_probe_obligations().unwrap(), 1);
}
