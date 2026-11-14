use store::{
    Store, StoredFingerprintAttempt, StoredFingerprintModelBundle, StoredFingerprintSample,
};

pub fn capture(store: &Store, file: &domain::LibraryFile, episode: u32) -> StoredFingerprintSample {
    let source = marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/48000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture".into(),
    };
    store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &library::Tracks {
                video: Some(Default::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    capture_source: Some(source.clone()),
                    is_default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(&file.version.source_version),
            Some(1_200_000),
        )
        .unwrap();
    let metrics = marker::CaptureMetrics {
        audio_stream_index: Some(0),
        pcm_timeline: marker::PcmTimeline::Contiguous,
        contiguous_pcm_start_ms: Some(0),
        time_mapping_version: 3,
        source_facts: Some(source.clone()),
        ..Default::default()
    };
    let sample = StoredFingerprintSample {
        sample_id: format!("sample-{episode}"),
        ledger_id: file.file_id.to_string(),
        source_version: file.version.source_version.clone(),
        capture_profile_key: marker::capture_profile::capture_profile_key(
            &marker::capture_profile::profile_for_source(Some(&source)),
        ),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 180_000,
        pcm_duration_ms: Some(180_000),
        fingerprint: vec![11, 22, 33, episode],
        captured_job_id: "capture-job".into(),
        captured_at_ms: 1000,
        metrics_json: serde_json::to_string(&metrics).unwrap(),
    };
    save(store, file, &sample);
    sample
}

pub fn save(store: &Store, file: &domain::LibraryFile, sample: &StoredFingerprintSample) {
    let attempt = StoredFingerprintAttempt {
        attempt_id: format!("attempt-{}", sample.sample_id),
        job_id: sample.captured_job_id.clone(),
        ledger_id: sample.ledger_id.clone(),
        kind: sample.kind.clone(),
        window_start_ms: sample.window_start_ms,
        window_end_ms: sample.window_end_ms,
        phase: "seed_capture".into(),
        started_at_ms: 1,
        finished_at_ms: Some(2),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: sample.metrics_json.clone(),
    };
    assert!(
        store
            .complete_fingerprint_attempt_for_source(&attempt, sample, &file.path.to_string_lossy())
            .unwrap()
    );
}

pub fn bundle(
    media: domain::MediaId,
    samples: &[StoredFingerprintSample],
) -> StoredFingerprintModelBundle {
    let template = marker::adaptive::TemplateModel {
        model_id: String::new(),
        version: marker::adaptive::TEMPLATE_MODEL_VERSION,
        kind: marker::SegmentKind::Intro,
        references: samples
            .iter()
            .enumerate()
            .map(|(index, sample)| marker::adaptive::TemplateReference {
                sample_id: sample.sample_id.clone(),
                ledger_id: sample.ledger_id.clone(),
                episode: index as u32 + 1,
                source_version: sample.source_version.clone(),
                match_interval_ms: (30_000, 90_000),
                match_from_end_ms: None,
            })
            .collect(),
        expected_duration_ms: 60_000,
        is_stable: false,
    };
    StoredFingerprintModelBundle::new(media, 1, "policy".into(), template, samples.to_vec(), 1000)
        .unwrap()
}
