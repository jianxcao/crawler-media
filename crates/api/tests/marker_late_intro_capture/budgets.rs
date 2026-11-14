use super::*;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_interrupted_and_running_receipts_consume_the_cumulative_budget() {
    let (s, engine) = scenario(3, -1, 0, None).await;
    let store = s.store.clone();
    let rows = s.ledgers.clone();
    *engine.on_capture.lock().unwrap() = Some(Arc::new(move |request| {
        if request.window.start_ms != 0 {
            return;
        }
        let row = rows.iter().find(|r| r.file.path == request.path).unwrap();
        let unit = store
            .active_probe_unit_for_ledger(&row.file.file_id.to_string())
            .unwrap()
            .unwrap();
        for status in ["failed", "interrupted", "running"] {
            let attempt = store::StoredFingerprintAttempt {
                attempt_id: format!("{}-{status}", row.file.file_id),
                job_id: unit.job_id.clone(),
                ledger_id: row.file.file_id.to_string(),
                kind: "intro".into(),
                window_start_ms: 0,
                window_end_ms: 180_000,
                phase: "recovered_fixture".into(),
                started_at_ms: 1,
                finished_at_ms: None,
                status: status.into(),
                error_kind: None,
                metrics_json: "{}".into(),
            };
            store.begin_fingerprint_attempt(&attempt).unwrap();
        }
    }));
    let job = run(&s).await;
    assert_eq!(
        job.status, "succeeded",
        "exhausted search must stay typed uncertain: {job:?}"
    );
    assert_windows(&s, &engine, &[(0, 180_000)]);
    for row in &s.ledgers {
        let attempts = s.store.list_fingerprint_attempts_for_job(&job.id).unwrap();
        assert_eq!(
            attempts
                .iter()
                .filter(|a| a.ledger_id == row.file.file_id.to_string() && a.kind == "intro")
                .count(),
            4
        );
        assert!(
            s.store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_none()
        );
        let unit = s
            .store
            .probe_job_units(&job.id)
            .unwrap()
            .into_iter()
            .find(|u| u.ledger_id == row.file.file_id.to_string())
            .unwrap();
        let outcome: serde_json::Value =
            serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
        assert_eq!(outcome["kinds"]["intro"]["status"], "uncertain");
        assert!(
            outcome["kinds"]["intro"]["reason"]
                .as_str()
                .unwrap()
                .contains("budget")
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_files_three_windows_drain_with_bounded_comparison_work() {
    let (s, engine) = scenario(8, -1, 0, None).await;
    let job = run(&s).await;
    assert_eq!(job.status, "succeeded");
    assert_windows(
        &s,
        &engine,
        &[(0, 180_000), (120_000, 420_000), (300_000, 600_000)],
    );
    let work = s.manager.timing_snapshot(&job.id).unwrap();
    assert!(
        work.raw_pairs_computed.is_some_and(|n| n > 0 && n <= 450),
        "{work:?}"
    );
    assert!(s.store.recover_probe_jobs().unwrap().is_empty());
    assert_eq!(
        s.store
            .list_fingerprint_attempts_for_job(&job.id)
            .unwrap()
            .len(),
        8 * 4
    );
}
