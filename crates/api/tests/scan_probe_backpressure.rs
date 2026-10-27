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
    let store = store_arc.lock();
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
                .lock()
                .active_probe_unit_for_ledger(&ledger_id(row))
                .unwrap()
                .expect("every scanned file must have durable probe work")
                .job_id
        })
        .collect();
    assert!(store.lock().scan_probe_obligations(100).unwrap().is_empty());
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
            .lock()
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
    let recovered = std::sync::Arc::new(ProbeManager::new(store.clone()));
    assert!(recovered.queued_ledger_count() <= 64);
    assert!(recovered.try_start_workers());
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if jobs
                .iter()
                .all(|id| store.lock().get_probe_job(id).unwrap().unwrap().status == "succeeded")
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
        fix.state
            .store()
            .lock()
            .scan_probe_obligations(10)
            .unwrap()[0]
            .ledger_id,
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
            .lock()
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
            .lock()
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
