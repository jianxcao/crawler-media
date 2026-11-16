use super::*;

pub(super) fn seed_narrow_intro(store: &Store, row: &FileCoverage, episode: u32) {
    // Stable identities put the earlier narrow capture before the later full
    // capture. Discovery must use usable evidence regardless of random UUID order.
    let identity = format!("000-narrow-{episode}");
    let metrics = marker::CaptureMetrics {
        audio_stream_index: Some(0),
        pcm_timeline: marker::PcmTimeline::Contiguous,
        contiguous_pcm_start_ms: Some(0),
        source_facts: Some(source_facts()),
        time_mapping_version: 3,
        ..Default::default()
    };
    let sample = store::StoredFingerprintSample {
        sample_id: identity.clone(),
        ledger_id: row.file.file_id.to_string(),
        source_version: row.file.version.source_version.clone(),
        capture_profile_key: api::fingerprint_job::capture_profile_key(
            &api::fingerprint_job::profile_for_source(Some(&source_facts())),
        ),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 100_000,
        pcm_duration_ms: Some(100_000),
        fingerprint: vec![0, episode, 42, 43],
        captured_job_id: identity.clone(),
        captured_at_ms: 1,
        metrics_json: serde_json::to_string(&metrics).unwrap(),
    };
    let attempt = store::StoredFingerprintAttempt {
        attempt_id: identity.clone(),
        job_id: identity,
        ledger_id: sample.ledger_id.clone(),
        kind: sample.kind.clone(),
        window_start_ms: sample.window_start_ms,
        window_end_ms: sample.window_end_ms,
        phase: "fixture".into(),
        started_at_ms: 0,
        finished_at_ms: Some(1),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: sample.metrics_json.clone(),
    };
    store
        .complete_fingerprint_attempt(&attempt, Some(&sample))
        .unwrap();
}
