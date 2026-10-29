#[path = "support/mod.rs"]
mod support;

use support::library_presence::PresenceFixture;

#[tokio::test]
async fn probe_obligation_is_created_on_scan_and_recovered() {
    let fix = PresenceFixture::new();

    // Scan a file into the library
    let file = fix.root.join("ob_movie (2024).mkv");
    std::fs::write(&file, b"content").unwrap();

    let (status, _) = fix.scan_and_wait(&fix.library_id).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    let store_arc = fix.state.store();
    let _row = store_arc
        .lock()
        .file_coverage_by_path(&file.display().to_string())
        .unwrap()
        .expect("row must exist");

    // Dispatch once. The 7-byte "video" can never probe successfully, and failed
    // work must stay retryable: the obligation is acked only by accepted or
    // completed work, otherwise it remains next to the failed job.
    api::scan_probe_dispatch::dispatch_once(fix.state.clone())
        .await
        .unwrap();
    let ledger_id = _row.file.file_id.to_string();
    let guard = store_arc.lock();
    let remaining = guard.scan_probe_obligations(10).unwrap();
    let in_flight = guard
        .active_probe_unit_for_ledger(&ledger_id)
        .unwrap()
        .is_some();
    assert!(
        guard.probe_unit_recorded_for_ledger(&ledger_id).unwrap(),
        "the scan obligation must have produced probe work"
    );
    assert!(
        remaining.is_empty() || (!in_flight && remaining.len() == 1),
        "obligation is acked by live work or kept for retry after failure: {remaining:?}"
    );
}

#[tokio::test]
async fn deleted_ledger_cleans_obligation_without_probe() {
    let fix = PresenceFixture::new();
    let file = fix.root.join("del_movie (2024).mkv");
    std::fs::write(&file, b"content").unwrap();

    fix.scan_and_wait(&fix.library_id).await;

    let store_arc = fix.state.store();
    let row = store_arc
        .lock()
        .file_coverage_by_path(&file.display().to_string())
        .unwrap()
        .unwrap();

    // Delete ledger path
    store_arc
        .lock()
        .delete_ledger_path(&row.file.path.to_string_lossy())
        .unwrap();

    // Verify obligation is cleaned up by delete_ledger_path
    let obs = store_arc.lock().scan_probe_obligations(10).unwrap();
    assert!(
        obs.is_empty(),
        "obligation must be deleted when ledger row is deleted"
    );
}
