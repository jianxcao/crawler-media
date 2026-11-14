use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_intro_model_revisits_partial_files_with_same_reference_evidence() {
    let mut s = scenario("adaptive").await;
    let first = run(&s, true).await;
    assert_eq!(outcome(&s, &first)["status"], "partial");
    let other = &s.ledgers[1];
    let before = s
        .store
        .latest_probe_job_for_ledger(&other.file.file_id.to_string())
        .unwrap()
        .unwrap();
    let before_marker = s
        .store
        .get_file_marker(other.file.file_id, other.units[0])
        .unwrap()
        .unwrap();
    assert_eq!(before_marker.intro_end_ms, Some(90_000));
    restart(
        &mut s,
        Arc::new(KindEngine {
            intro_secs: 80.0,
            outro_secs: 3.0,
        }),
    );
    assert!(s.manager.enqueue(unit(&s.ledgers[0])));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !s.store.recover_probe_jobs().unwrap().is_empty() {
            s.clock.set(s.clock.now_ms() + 2_000);
            s.manager.wake_dispatch();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    s.manager.shutdown_workers().await.unwrap();
    let other = &s.ledgers[1];
    let after = s
        .store
        .latest_probe_job_for_ledger(&other.file.file_id.to_string())
        .unwrap()
        .unwrap();
    assert_ne!(
        after, before,
        "a changed accepted-kind model must create a successor for a partial file"
    );
    let units = s.store.probe_job_units(&after).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(units[0].detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["kinds"]["intro"]["status"], "verified");
    let previous: serde_json::Value = serde_json::from_str(
        s.store
            .probe_job_units(&before)
            .unwrap()
            .iter()
            .find(|u| u.ledger_id == other.file.file_id.to_string())
            .unwrap()
            .detection_outcome_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        result["available_reference_key"], previous["available_reference_key"],
        "followup must be caused by changed models, not changed capture evidence"
    );
    assert_ne!(
        result["model_reference_key"],
        previous["model_reference_key"]
    );
    let marker = s
        .store
        .get_file_marker(other.file.file_id, other.units[0])
        .unwrap()
        .unwrap();
    assert_eq!(marker.intro_end_ms, Some(110_000));
    assert_eq!(marker.outro_start_ms, before_marker.outro_start_ms);
}
