#[path = "support/mod.rs"]
mod support;

use api::probe_manager::{ProbeManager, policy::ProbeRequestOrigin};
use domain::{
    Confidence, FileCoverage, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::ScannedLedger;
use support::library_presence::PresenceFixture;

fn fixture_without_workers() -> PresenceFixture {
    // ApiState starts background services when constructed inside a runtime.
    // Keep admission deterministic; workers start only after fixture caches exist.
    std::thread::spawn(PresenceFixture::new).join().unwrap()
}

/// Insert one scanned file (commit + durable probe obligation) for its own Media.
///
/// The bytes exist on disk so later source checks and the fingerprint stage can
/// read a real version, while the library fixture keeps the file inside its root.
fn insert_scanned(fix: &PresenceFixture, index: usize) -> FileCoverage {
    let path = fix.root.join(format!("movie-{index}.strm"));
    std::fs::write(&path, "http://offline.invalid/video").unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: format!("Scanned Movie {index}"),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let file_id = LedgerId::new();
    // A real scan records the version it measured; the probe worker re-observes the source
    // and publishes only while the recorded version still matches.
    let source_version = api::fingerprint_job::current_source_version(&path);
    let store_arc = fix.state.store();
    let store = store_arc.as_ref();
    store.insert_media(&media).unwrap();
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id,
                media_id: media.id,
                path: path.display().to_string(),
                units: vec![UnitKey::WHOLE],
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version,
                source_digest: None,
                size_bytes: None,
                created_ns: None,
                modified_ns: None,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            index as i64,
        )
        .unwrap();
    store
        .file_coverage_by_id(file_id)
        .unwrap()
        .expect("scanned file row")
}

fn scanned_rows(fix: &PresenceFixture, count: usize) -> Vec<FileCoverage> {
    (0..count).map(|index| insert_scanned(fix, index)).collect()
}

fn ledger_id(row: &FileCoverage) -> String {
    row.file.file_id.to_string()
}

#[tokio::test]
async fn scan_backlog_survives_full_window_and_restart_then_drains() {
    let fix = fixture_without_workers();
    let rows = scanned_rows(&fix, 130);
    for _ in 0..3 {
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap();
    }
    let store = fix.state.store();
    let jobs: Vec<_> = rows
        .iter()
        .map(|row| {
            store
                .as_ref()
                .active_probe_unit_for_ledger(&ledger_id(row))
                .unwrap()
                .expect("every scanned file must have durable probe work")
                .job_id
        })
        .collect();
    assert!(
        store
            .as_ref()
            .scan_probe_obligations(100)
            .unwrap()
            .is_empty()
    );
    assert!(fix.state.probe.queued_ledger_count() <= 64);
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        0
    );

    // A fresh manager must recover the overflow, even though its scan obligation
    // was already acknowledged. Supply fixture metadata so no media/network IO runs.
    for row in &rows {
        let version = api::fingerprint_job::current_source_version(&row.file.path);
        store
            .as_ref()
            .put_file_meta_versioned(
                &ledger_id(row),
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    ..Default::default()
                },
                Some(&version),
                Some(120_000),
            )
            .unwrap();
    }
    let recovered = std::sync::Arc::new(ProbeManager::new(store.clone()).unwrap());
    assert!(recovered.queued_ledger_count() <= 64);
    assert!(recovered.try_start_workers());
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if jobs
                .iter()
                .all(|id| store.as_ref().get_probe_job(id).unwrap().unwrap().status == "succeeded")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("durable overflow must drain without detail requests or rescans");
    recovered.shutdown_workers().await.unwrap();
    assert!(rows.iter().all(|row| !recovered.is_queued(&ledger_id(row))));
}

#[tokio::test]
async fn rejected_persistence_does_not_ack_scan_obligation() {
    let fix = fixture_without_workers();
    let row = scanned_rows(&fix, 1).remove(0);
    let connection = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_probe BEFORE INSERT ON probe_jobs
         BEGIN SELECT RAISE(ABORT, 'fixture persistence unavailable'); END;",
        )
        .unwrap();
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        fix.state.store().scan_probe_obligations(10).unwrap()[0].ledger_id,
        row.file.file_id
    );
    connection
        .execute_batch("DROP TRIGGER reject_probe")
        .unwrap();
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        1
    );
    assert!(
        fix.state
            .store()
            .active_probe_unit_for_ledger(&ledger_id(&row))
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn durable_overflow_can_ack_without_an_in_memory_dispatch_id() {
    let fix = fixture_without_workers();
    let rows = scanned_rows(&fix, 65);
    for row in &rows {
        fix.state
            .probe
            .request_probe(row, ProbeRequestOrigin::Filesystem)
            .unwrap();
    }
    assert!(!fix.state.probe.holds_dispatch_id(&ledger_id(&rows[64])));
    assert!(
        fix.state
            .store()
            .active_probe_unit_for_ledger(&ledger_id(&rows[64]))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        65
    );
}

fn insert_tv_with_failed_outro(
    fix: &PresenceFixture,
    tv_root: &std::path::Path,
    index: usize,
    failures: i64,
) {
    let store = fix.state.store();
    let path = tv_root.join(format!("Show.S01E{index:02}.mkv"));
    std::fs::write(&path, b"tv-bytes").unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: format!("Waiting Show {index}"),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let file_id = LedgerId::new();
    let source_version = api::fingerprint_job::current_source_version(&path);
    store.insert_media(&media).unwrap();
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id,
                media_id: media.id,
                path: path.display().to_string(),
                units: vec![UnitKey::new(1, index as i32 + 1)],
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version: source_version.clone(),
                source_digest: None,
                size_bytes: None,
                created_ns: None,
                modified_ns: None,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            index as i64,
        )
        .unwrap();
    let row = store.file_coverage_by_id(file_id).unwrap().unwrap();
    let lid = file_id.to_string();
    let tracks = library::Tracks {
        video: Some(Default::default()),
        ..Default::default()
    };
    store
        .put_file_meta_versioned(&lid, &tracks, Some(&source_version), Some(600_000))
        .unwrap();
    let source = api::fingerprint_job::capture_source(&tracks, None).ok();
    let profile = api::fingerprint_job::profile_for_source(source.as_ref());
    let context_key = api::fingerprint_job::fingerprint_cache_key_with_profile(
        &row.file.version.source_version,
        180,
        Some(600_000),
        &profile,
    );
    store
        .put_fingerprint_cache(
            &lid,
            &store::FingerprintCacheEntry {
                cache_key: context_key.clone(),
                algorithm_version: marker::FINGERPRINT_ALGORITHM_VERSION,
                sample_duration_secs: 180,
                media_duration_ms: Some(600_000),
                intro: marker::CapturedFingerprint {
                    window: marker::SampleWindow::new(0, 180_000),
                    words: vec![1, 2, 3],
                    pcm_duration_ms: Some(180_000),
                    metrics: Default::default(),
                },
                outro: None,
            },
        )
        .unwrap();
    let outro_key = store::ProbeStageKey {
        ledger_id: lid,
        context_key,
        stage: store::ProbeStage::Outro,
    };
    let now_ms = store::unix_now() * 1000;
    let base_ms = now_ms - 10 * 86_400_000;
    for attempt in 0..failures {
        let job = format!("job-{index}-{attempt}");
        let ts = if attempt == failures - 1 && failures < 5 {
            now_ms
        } else {
            base_ms + attempt * 86_400_000
        };
        assert!(store.claim_probe_stage(&outro_key, &job, ts).unwrap());
        assert!(
            store
                .finish_probe_stage(
                    &outro_key,
                    &job,
                    &store::ProbeStageCompletion::Failed {
                        error_kind: "network".into(),
                        error: "boom".into(),
                    },
                    ts,
                )
                .unwrap()
        );
    }
}

#[tokio::test]
async fn waiting_head_obligations_do_not_starve_later_obligations() {
    let fix = fixture_without_workers();
    let store = fix.state.store();
    let tv_root = fix.dir.path().join("shows");
    std::fs::create_dir_all(&tv_root).unwrap();
    let tv_lib = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&tv_lib.id, true, true)
        .unwrap();
    for index in 0..100 {
        insert_tv_with_failed_outro(&fix, &tv_root, index, 1);
    }
    let tail = insert_scanned(&fix, 100);
    let tail_id = tail.file.file_id.to_string();
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        1,
        "cursor pagination must reach obligations behind 100 Waiting rows"
    );
    assert!(
        store
            .active_probe_unit_for_ledger(&tail_id)
            .unwrap()
            .is_some(),
        "row #101 behind 100 Waiting rows must get durable probe work"
    );
    assert_eq!(store.scan_probe_obligations(105).unwrap().len(), 100);
}

#[tokio::test]
async fn exhausted_obligation_is_acknowledged_and_cleared() {
    let fix = fixture_without_workers();
    let store = fix.state.store();
    let tv_root = fix.dir.path().join("shows");
    std::fs::create_dir_all(&tv_root).unwrap();
    let tv_lib = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&tv_lib.id, true, true)
        .unwrap();
    insert_tv_with_failed_outro(&fix, &tv_root, 1, 5);
    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(fix.state.clone())
            .await
            .unwrap(),
        1,
        "Exhausted obligation must be acknowledged"
    );
    assert!(store.scan_probe_obligations(10).unwrap().is_empty());
}
