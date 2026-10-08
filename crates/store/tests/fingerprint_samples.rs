use store::{
    FingerprintSampleQuery, Store, StoredFingerprintModel, StoredFingerprintModelMember,
    StoredFingerprintSample,
};

fn sample(
    sample_id: &str,
    ledger_id: &str,
    source_version: &str,
    profile_key: &str,
    kind: &str,
    start_ms: i64,
    end_ms: i64,
    job_id: &str,
) -> StoredFingerprintSample {
    StoredFingerprintSample {
        sample_id: sample_id.into(),
        ledger_id: ledger_id.into(),
        source_version: source_version.into(),
        capture_profile_key: profile_key.into(),
        kind: kind.into(),
        window_start_ms: start_ms,
        window_end_ms: end_ms,
        pcm_duration_ms: Some(end_ms - start_ms),
        fingerprint: vec![10, 20, 30, 40],
        captured_job_id: job_id.into(),
        captured_at_ms: 1_000_000,
        metrics_json: "{}".into(),
    }
}

#[test]
fn samples_survive_reopen_and_preserve_absolute_windows() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");

    let store = Store::open(&data).unwrap();
    let s = sample(
        "sample-1",
        "ledger-1",
        "src-v1",
        "prof-1",
        "intro",
        100_000,
        140_000,
        "job-1",
    );
    let attempt = store::StoredFingerprintAttempt {
        attempt_id: "attempt-1".into(),
        job_id: "job-1".into(),
        ledger_id: "ledger-1".into(),
        kind: "intro".into(),
        window_start_ms: 100_000,
        window_end_ms: 140_000,
        phase: "seed_capture".into(),
        started_at_ms: 1_000_000,
        finished_at_ms: Some(1_005_000),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    store.begin_fingerprint_attempt(&attempt).unwrap();
    store
        .complete_fingerprint_attempt(&attempt, Some(&s))
        .unwrap();
    drop(store);

    let store = Store::open(&data).unwrap();
    let query = FingerprintSampleQuery {
        ledger_id: "ledger-1".into(),
        source_version: "src-v1".into(),
        capture_profile_key: "prof-1".into(),
        kind: "intro".into(),
        window_start_ms: 100_000,
        window_end_ms: 140_000,
        captured_job_id: None,
    };
    let reopened = store.find_covering_fingerprint_samples(&query).unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].window_start_ms, 100_000);
    assert_eq!(reopened[0].window_end_ms, 140_000);
    assert_eq!(reopened[0].fingerprint, vec![10, 20, 30, 40]);

    let changed_query = FingerprintSampleQuery {
        ledger_id: "ledger-1".into(),
        source_version: "src-v2".into(),
        capture_profile_key: "prof-1".into(),
        kind: "intro".into(),
        window_start_ms: 100_000,
        window_end_ms: 140_000,
        captured_job_id: None,
    };
    let changed_source_matches = store
        .find_covering_fingerprint_samples(&changed_query)
        .unwrap();
    assert!(changed_source_matches.is_empty());
}

#[test]
fn covering_sample_requires_same_source_profile_kind_and_job() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("data")).unwrap();

    let s = sample(
        "sample-1",
        "ledger-1",
        "src-v1",
        "prof-1",
        "intro",
        0,
        180_000,
        "job-1",
    );
    let attempt = store::StoredFingerprintAttempt {
        attempt_id: "attempt-1".into(),
        job_id: "job-1".into(),
        ledger_id: "ledger-1".into(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 180_000,
        phase: "seed_capture".into(),
        started_at_ms: 1000,
        finished_at_ms: Some(2000),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    store.begin_fingerprint_attempt(&attempt).unwrap();
    store
        .complete_fingerprint_attempt(&attempt, Some(&s))
        .unwrap();

    // Covering query: range [10_000, 50_000] falls within [0, 180_000]
    let query_covered = FingerprintSampleQuery {
        ledger_id: "ledger-1".into(),
        source_version: "src-v1".into(),
        capture_profile_key: "prof-1".into(),
        kind: "intro".into(),
        window_start_ms: 10_000,
        window_end_ms: 50_000,
        captured_job_id: None,
    };
    let hits = store
        .find_covering_fingerprint_samples(&query_covered)
        .unwrap();
    assert_eq!(hits.len(), 1);

    // Job mismatch query: Recapture specifies Some("job-2") but sample was captured by job-1
    let query_job_mismatch = FingerprintSampleQuery {
        captured_job_id: Some("job-2".into()),
        ..query_covered.clone()
    };
    assert!(
        store
            .find_covering_fingerprint_samples(&query_job_mismatch)
            .unwrap()
            .is_empty()
    );

    // Kind mismatch
    let query_kind_mismatch = FingerprintSampleQuery {
        kind: "outro".into(),
        ..query_covered.clone()
    };
    assert!(
        store
            .find_covering_fingerprint_samples(&query_kind_mismatch)
            .unwrap()
            .is_empty()
    );

    // Window exceeds bounds: [0, 200_000] does not fit inside [0, 180_000]
    let query_out_of_bounds = FingerprintSampleQuery {
        window_end_ms: 200_000,
        ..query_covered
    };
    assert!(
        store
            .find_covering_fingerprint_samples(&query_out_of_bounds)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn deleting_file_metadata_invalidates_samples_and_referencing_models() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("data")).unwrap();

    let s1 = sample(
        "s-1", "ledger-1", "src-1", "p-1", "intro", 0, 180_000, "job-1",
    );
    let s2 = sample(
        "s-2", "ledger-2", "src-1", "p-1", "intro", 0, 180_000, "job-1",
    );
    let attempt1 = store::StoredFingerprintAttempt {
        attempt_id: "att-1".into(),
        job_id: "job-1".into(),
        ledger_id: "ledger-1".into(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 180_000,
        phase: "seed_capture".into(),
        started_at_ms: 1000,
        finished_at_ms: Some(2000),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    let attempt2 = store::StoredFingerprintAttempt {
        attempt_id: "att-2".into(),
        ledger_id: "ledger-2".into(),
        ..attempt1.clone()
    };
    store.begin_fingerprint_attempt(&attempt1).unwrap();
    store
        .complete_fingerprint_attempt(&attempt1, Some(&s1))
        .unwrap();
    store.begin_fingerprint_attempt(&attempt2).unwrap();
    store
        .complete_fingerprint_attempt(&attempt2, Some(&s2))
        .unwrap();

    let model = StoredFingerprintModel {
        model_id: "model-1".into(),
        media_id: "media-1".into(),
        season: 1,
        kind: "intro".into(),
        model_version: 1,
        membership_key: "key-1".into(),
        policy_key: "pol-1".into(),
        model_json: "{}".into(),
        created_at_ms: 2000,
    };
    let members = [
        StoredFingerprintModelMember {
            model_id: "model-1".into(),
            sample_id: "s-1".into(),
            ledger_id: "ledger-1".into(),
            source_version: "src-1".into(),
        },
        StoredFingerprintModelMember {
            model_id: "model-1".into(),
            sample_id: "s-2".into(),
            ledger_id: "ledger-2".into(),
            source_version: "src-1".into(),
        },
    ];
    store.put_fingerprint_model(&model, &members).unwrap();

    let models = store.list_fingerprint_models("media-1", 1).unwrap();
    assert_eq!(models.len(), 1);

    // Delete file meta of ledger-1 -> should invalidate sample s-1 and referencing model-1
    store.delete_file_meta_by_ledger_id("ledger-1").unwrap();

    let q = FingerprintSampleQuery {
        ledger_id: "ledger-1".into(),
        source_version: "src-1".into(),
        capture_profile_key: "p-1".into(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 180_000,
        captured_job_id: None,
    };
    assert!(
        store
            .find_covering_fingerprint_samples(&q)
            .unwrap()
            .is_empty()
    );

    let remaining_models = store.list_fingerprint_models("media-1", 1).unwrap();
    assert!(remaining_models.is_empty());
}
