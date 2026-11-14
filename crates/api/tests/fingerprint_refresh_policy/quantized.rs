use super::*;

fn source(rate: u32) -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 1,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/1000".into()),
        codec_name: Some("aac".into()),
        native_sample_rate: Some(rate),
        audio_channels: Some(2),
        provenance: "fixture_header_v2".into(),
    }
}

fn ranged_capture(capture: &mut marker::CapturedFingerprint, rate: u32) {
    let start = capture.window.start_ms * 1000;
    let count = capture.window.duration_ms() as u64 * 16;
    capture.metrics.time_mapping_version = 4;
    capture.metrics.pcm_timeline = marker::PcmTimeline::Contiguous;
    capture.metrics.contiguous_pcm_start_ms = None;
    capture.metrics.audio_stream_index = Some(1);
    capture.metrics.source_facts = Some(source(rate));
    capture.metrics.sample_count = Some(count);
    capture.metrics.pcm_bytes = count * 2;
    let packets = if rate == 48_000 { 8440 } else { 7800 };
    capture.metrics.quantized_mapping = Some(marker::QuantizedPcmMapping {
        origin_min_us: start - 500,
        origin_max_us: start + 500,
        native_sample_rate: rate,
        source_tick_us: 1000,
        packet_count: packets,
        block_anchors: 975,
        codec_native_samples: packets * 1024,
        decoded_native_samples: packets * 1024,
        encoded_payload_sha256: "a".repeat(64),
        proof_version: 1,
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quantized_proof_reuses_without_audio_and_missing_proof_requires_recapture() {
    check_quantized_reuse(44_100).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quantized_aac48_reuses_without_audio_and_rate_mismatch_requires_recapture() {
    check_quantized_reuse(48_000).await;
}

async fn check_quantized_reuse(rate: u32) {
    let mut s = support::ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        let mut tracks = s.store.get_file_meta(&id).unwrap().unwrap();
        tracks.audio[0].stream_index = Some(1);
        tracks.audio[0].capture_source = Some(source(rate));
        s.store
            .put_file_meta_versioned(
                &id,
                &tracks,
                Some(&row.file.version.source_version),
                Some(2_400_000),
            )
            .unwrap();
        let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
        cache.cache_key = fingerprint_cache_key_with_profile(
            &row.file.version.source_version,
            180,
            Some(2_400_000),
            &profile_for_source(Some(&source(rate))),
        );
        ranged_capture(&mut cache.intro, rate);
        ranged_capture(cache.outro.as_mut().unwrap(), rate);
        s.store.put_fingerprint_cache(&id, &cache).unwrap();
    }
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(RefuseCapture),
            Arc::new(RefuseCapture),
        )
        .unwrap(),
    );
    assert!(s.manager.try_start_workers());
    assert!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ) > 0
    );
    assert_eq!(terminal(&s).await.status, "succeeded");
    let row = &s.ledgers[0];
    let old = s
        .store
        .get_file_marker(row.file.file_id, row.units[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        (old.intro_start_ms, old.intro_end_ms),
        (Some(30_001), Some(89_999))
    );
    let id = row.file.file_id.to_string();
    let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
    if rate == 48_000 {
        cache
            .intro
            .metrics
            .quantized_mapping
            .as_mut()
            .unwrap()
            .native_sample_rate = 44_100;
    } else {
        cache.intro.metrics.quantized_mapping = None;
    }
    s.store.put_fingerprint_cache(&id, &cache).unwrap();
    assert!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ) > 0
    );
    assert_eq!(terminal(&s).await.status, "failed");
    assert_eq!(
        s.store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .unwrap(),
        old
    );
    s.manager.shutdown_workers().await.unwrap();
}
