use super::*;

async fn invalid_capture_requires_recapture(invalidate: fn(&mut marker::CapturedFingerprint)) {
    let mut s = scenario().await;
    probe(&s).await;
    let old = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    let key = s.ledgers[0].file.file_id.to_string();
    let mut cached = s.store.get_fingerprint_cache(&key).unwrap().unwrap();
    invalidate(&mut cached.intro);
    s.store.put_fingerprint_cache(&key, &cached).unwrap();
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(FixedCandidate),
            Arc::new(NoRecapture),
        )
        .unwrap(),
    );
    assert!(s.manager.try_start_workers());
    probe(&s).await;
    let job = s
        .store
        .latest_probe_job_for_scope(&format!("ledger:{key}"))
        .unwrap()
        .unwrap();
    assert_eq!(
        job.status, "failed",
        "invalid capture must require new audio"
    );
    assert!(job.error.unwrap().contains("audio recapture forbidden"));
    assert_old(&s, &old);
    assert_eq!(
        s.store.get_fingerprint_cache(&key).unwrap().unwrap().intro,
        cached.intro,
        "failed capture cannot replace or restamp the previous evidence"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contradictory_known_pcm_origin_requires_recapture_and_preserves_previous() {
    invalid_capture_requires_recapture(|capture| {
        capture.metrics.contiguous_pcm_start_ms = Some(5_000);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_source_facts_require_recapture_and_preserve_previous() {
    invalid_capture_requires_recapture(|capture| capture.metrics.source_facts = None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_pcm_origin_requires_recapture_and_preserves_previous() {
    invalid_capture_requires_recapture(|capture| capture.metrics.contiguous_pcm_start_ms = None)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_pcm_timeline_requires_recapture_and_preserves_previous() {
    invalid_capture_requires_recapture(|capture| {
        capture.metrics.pcm_timeline = marker::PcmTimeline::Unknown;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discontinuous_pcm_requires_recapture_and_preserves_previous() {
    invalid_capture_requires_recapture(|capture| {
        capture.metrics.pcm_timeline = marker::PcmTimeline::Invalid("decoded gap".into());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contradictory_audio_identity_requires_recapture_and_preserves_previous() {
    invalid_capture_requires_recapture(|capture| capture.metrics.audio_stream_index = Some(1))
        .await;
}
