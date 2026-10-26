#[path = "support/mod.rs"]
mod support;

use api::probe_manager::{ProbeManager, policy::ProbeRequestOrigin};
use support::library_presence::PresenceFixture;

fn fixture_without_workers() -> PresenceFixture {
    // ApiState starts background services when constructed inside a runtime.
    // Keep admission deterministic; workers start only after fixture caches exist.
    std::thread::spawn(PresenceFixture::new).join().unwrap()
}

fn scanned_rows(fix: &PresenceFixture, count: usize) -> Vec<domain::LedgerRow> {
    (0..count)
        .map(|index| {
            let (_, row) = fix.insert_media_and_ledger(&format!("movie-{index}.strm"));
            std::fs::write(&row.path, "http://offline.invalid/video").unwrap();
            let store = fix.state.store();
            let store = store.lock();
            store.delete_ledger_path(&row.path).unwrap();
            store.insert_scanned_ledger(&row, index as i64).unwrap();
            row
        })
        .collect()
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
                .active_probe_unit_for_ledger(&row.id.to_string())
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
        let version = api::fingerprint_job::current_source_version(std::path::Path::new(&row.path));
        store
            .lock()
            .put_file_meta_versioned(
                &row.id.to_string(),
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
    assert!(
        rows.iter()
            .all(|row| !recovered.is_queued(&row.id.to_string()))
    );
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
        fix.state.store().lock().scan_probe_obligations(10).unwrap()[0].ledger_id,
        row.id
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
            .active_probe_unit_for_ledger(&row.id.to_string())
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
    assert!(!fix.state.probe.holds_dispatch_id(&rows[64].id.to_string()));
    assert!(
        fix.state
            .store()
            .lock()
            .active_probe_unit_for_ledger(&rows[64].id.to_string())
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
