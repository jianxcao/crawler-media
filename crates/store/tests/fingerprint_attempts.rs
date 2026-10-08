use store::{Store, StoredFingerprintAttempt, StoredFingerprintSample};

fn attempt(attempt_id: &str, status: &str) -> StoredFingerprintAttempt {
    StoredFingerprintAttempt {
        attempt_id: attempt_id.into(),
        job_id: "job-1".into(),
        ledger_id: "ledger-1".into(),
        kind: "intro".into(),
        window_start_ms: 100_000,
        window_end_ms: 140_000,
        phase: "seed_capture".into(),
        started_at_ms: 1_000,
        finished_at_ms: Some(2_000),
        status: status.into(),
        error_kind: None,
        metrics_json: "{\"input_bytes\":null,\"measurement_complete\":false}".into(),
    }
}

#[test]
fn attempt_and_sample_commit_together() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("data")).unwrap();

    let att = attempt("att-1", "running");
    store.begin_fingerprint_attempt(&att).unwrap();

    let completed_att = attempt("att-1", "succeeded");
    let sample = StoredFingerprintSample {
        sample_id: "s-1".into(),
        ledger_id: "ledger-1".into(),
        source_version: "v1".into(),
        capture_profile_key: "prof-1".into(),
        kind: "intro".into(),
        window_start_ms: 100_000,
        window_end_ms: 140_000,
        pcm_duration_ms: Some(40_000),
        fingerprint: vec![1, 2, 3],
        captured_job_id: "job-1".into(),
        captured_at_ms: 2_000,
        metrics_json: "{}".into(),
    };

    store
        .complete_fingerprint_attempt(&completed_att, Some(&sample))
        .unwrap();

    // Query sample
    let q = store::FingerprintSampleQuery {
        ledger_id: "ledger-1".into(),
        source_version: "v1".into(),
        capture_profile_key: "prof-1".into(),
        kind: "intro".into(),
        window_start_ms: 100_000,
        window_end_ms: 140_000,
        captured_job_id: None,
    };
    let samples = store.find_covering_fingerprint_samples(&q).unwrap();
    assert_eq!(samples.len(), 1);
}

#[test]
fn interrupted_attempt_preserves_unknown_byte_measurement() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("data")).unwrap();

    let att = StoredFingerprintAttempt {
        attempt_id: "att-interrupted".into(),
        job_id: "job-crash".into(),
        ledger_id: "ledger-crash".into(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 180_000,
        phase: "seed_capture".into(),
        started_at_ms: 10_000,
        finished_at_ms: None,
        status: "running".into(),
        error_kind: None,
        metrics_json: "{\"input_bytes\":null,\"measurement_complete\":false}".into(),
    };
    store.begin_fingerprint_attempt(&att).unwrap();

    // Recover jobs / recover interrupted attempts
    store.recover_interrupted_fingerprint_attempts("job-crash").unwrap();

    let recovered = store.get_fingerprint_attempt("att-interrupted").unwrap().unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert!(recovered.metrics_json.contains("\"input_bytes\":null"));
    assert!(recovered.metrics_json.contains("\"measurement_complete\":false"));
}

#[test]
fn put_and_read_probe_sampling_plan_and_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("data")).unwrap();

    let unit = store::ProbeJobUnitSpec {
        ledger_id: "ledger-plan",
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    };
    store
        .create_probe_job("job-plan", "marker_refresh", "media-1", Some(1), "scope-1", &[unit])
        .unwrap();

    store
        .put_probe_sampling_plan("job-plan", "ledger-plan", "{\"mode\":\"adaptive\"}")
        .unwrap();
    store
        .put_probe_detection_outcome("job-plan", "ledger-plan", "{\"result\":\"detected\"}")
        .unwrap();

    let units = store.probe_job_units("job-plan").unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].sampling_plan_json.as_deref(), Some("{\"mode\":\"adaptive\"}"));
    assert_eq!(units[0].detection_outcome_json.as_deref(), Some("{\"result\":\"detected\"}"));
}
