use super::*;

pub fn source(rate: u32) -> marker::CaptureSourceFacts {
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

pub fn metadata(s: &retry::ProbeScenario, rate: u32) {
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        let before = s
            .store
            .file_coverage_by_id(row.file.file_id)
            .unwrap()
            .unwrap();
        s.store
            .put_file_meta_versioned(
                &id,
                &library::Tracks {
                    video: Some(Default::default()),
                    audio: vec![library::AudioTrack {
                        sample_rate: Some(rate.to_string()),
                        stream_index: Some(1),
                        capture_source: Some(source(rate)),
                        is_default: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Some(&row.file.version.source_version),
                Some(2_400_000),
            )
            .unwrap();
        assert_eq!(
            s.store
                .file_coverage_by_id(row.file.file_id)
                .unwrap()
                .unwrap()
                .file
                .version,
            before.file.version
        );
    }
}

pub fn capture(window: marker::SampleWindow) -> marker::CapturedFingerprint {
    let count = window.duration_ms() as u64 * 16;
    let start = window.start_ms * 1000;
    marker::CapturedFingerprint {
        words: vec![1, 2, 3, 4],
        pcm_duration_ms: Some(window.duration_ms()),
        window,
        metrics: marker::CaptureMetrics {
            time_mapping_version: 4,
            audio_stream_index: Some(1),
            source_facts: Some(source(48_000)),
            pcm_timeline: marker::PcmTimeline::Contiguous,
            sample_count: Some(count),
            pcm_bytes: count * 2,
            quantized_mapping: Some(marker::QuantizedPcmMapping {
                origin_min_us: start - 500,
                origin_max_us: start + 500,
                native_sample_rate: 48_000,
                source_tick_us: 1000,
                packet_count: 8440,
                block_anchors: 1055,
                codec_native_samples: 8440 * 1024,
                decoded_native_samples: 8440 * 1024,
                encoded_payload_sha256: "a".repeat(64),
                proof_version: 1,
            }),
            ..Default::default()
        },
    }
}

pub async fn scenario() -> retry::ProbeScenario {
    let s = retry::ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    metadata(&s, 48_000);
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        let cache = store::FingerprintCacheEntry {
            cache_key: cache_key(row),
            algorithm_version: marker::FINGERPRINT_ALGORITHM_VERSION,
            sample_duration_secs: 180,
            media_duration_ms: Some(2_400_000),
            intro: capture(marker::SampleWindow::new(0, 180_000)),
            outro: Some(capture(marker::SampleWindow::new(2_220_000, 2_400_000))),
        };
        s.store.put_fingerprint_cache(&id, &cache).unwrap();
        for (kind, capture) in [
            ("intro", &cache.intro),
            ("outro", cache.outro.as_ref().unwrap()),
        ] {
            let metrics = serde_json::to_string(&capture.metrics).unwrap();
            let sample = store::StoredFingerprintSample {
                sample_id: format!("{id}-{kind}"),
                ledger_id: id.clone(),
                source_version: row.file.version.source_version.clone(),
                capture_profile_key: profile_key(),
                kind: kind.into(),
                window_start_ms: capture.window.start_ms,
                window_end_ms: capture.window.end_ms,
                pcm_duration_ms: capture.pcm_duration_ms,
                fingerprint: capture.words.clone(),
                captured_job_id: "old48".into(),
                captured_at_ms: 1000,
                metrics_json: metrics.clone(),
            };
            let attempt = store::StoredFingerprintAttempt {
                attempt_id: sample.sample_id.clone(),
                job_id: "old48".into(),
                ledger_id: id.clone(),
                kind: kind.into(),
                window_start_ms: capture.window.start_ms,
                window_end_ms: capture.window.end_ms,
                phase: "fixture".into(),
                started_at_ms: 1,
                finished_at_ms: Some(2),
                status: "succeeded".into(),
                error_kind: None,
                metrics_json: metrics,
            };
            assert!(
                s.store
                    .complete_fingerprint_attempt_for_source(
                        &attempt,
                        &sample,
                        row.file.path.to_str().unwrap()
                    )
                    .unwrap()
            );
        }
    }
    s
}

pub fn profile_key() -> String {
    capture_profile_key(&profile_for_source(Some(&source(48_000))))
}
pub fn cache_key(row: &domain::FileCoverage) -> String {
    fingerprint_cache_key_with_profile(
        &row.file.version.source_version,
        180,
        Some(2_400_000),
        &profile_for_source(Some(&source(48_000))),
    )
}

pub fn analysis_job(s: &retry::ProbeScenario, mode: &str, refresh: bool) -> String {
    let job = format!("analysis-{mode}");
    let ids = s
        .ledgers
        .iter()
        .map(|row| row.file.file_id.to_string())
        .collect::<Vec<_>>();
    let units = ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    assert!(
        s.store
            .create_probe_job(
                &job,
                if refresh {
                    "marker_refresh"
                } else {
                    "fingerprint_probe"
                },
                &s.media_id.to_string(),
                Some(1),
                &job,
                &units
            )
            .unwrap()
    );
    // This fixture isolates source qualification of existing captures. A single
    // admitted window per kind makes its actual replay budget already exhausted;
    // later-range discovery has separate public production coverage.
    let policy = marker::SamplingPolicy {
        max_windows_per_kind: if mode == "adaptive" { 1 } else { 2 },
        ..Default::default()
    };
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        let searches = if mode == "adaptive" {
            serde_json::json!([
                {"kind":"intro","windows":[{"start_ms":0,"end_ms":180_000}],"pending":null,"terminal_reason":null},
                {"kind":"outro","windows":[{"start_ms":2_220_000,"end_ms":2_400_000}],"pending":null,"terminal_reason":null}
            ])
        } else {
            serde_json::json!([])
        };
        let selected = if mode == "adaptive" {
            vec![format!("{id}-intro"), format!("{id}-outro")]
        } else {
            vec![]
        };
        let plan = serde_json::json!({"searches":searches,"selected_samples":selected,"policy":policy,"sampling_mode":mode});
        assert!(
            s.store
                .put_probe_sampling_plan(&job, &id, &plan.to_string())
                .unwrap()
        );
        assert!(s.store.start_probe_unit(&job, &id).unwrap());
        if refresh {
            assert!(
                s.store
                    .advance_probe_unit_to_fingerprint(&job, &id)
                    .unwrap()
            );
            assert!(s.store.start_probe_unit(&job, &id).unwrap());
        }
        let request = store::ProbeAnalysisRequest {
            dispatch: store::ProbeDispatchId {
                job_id: job.clone(),
                ledger_id: id.clone(),
                stage: store::ProbeDispatchStage::Fingerprint,
            },
            target: s
                .store
                .file_coverage_by_id(row.file.file_id)
                .unwrap()
                .unwrap(),
            season: 1,
            sampling_mode: mode.into(),
            analysis_policy_key: api::fingerprint_job::analysis_policy_key(&policy),
            analysis_policy_json: serde_json::to_string(&policy).unwrap(),
            capture_context_key: cache_key(row),
            required_kinds: vec![],
        };
        let prepared = s.store.prepare_probe_analysis(&request).unwrap();
        let prepared = if mode == "adaptive" {
            prepared
                .select_samples(&[format!("{id}-intro"), format!("{id}-outro")])
                .unwrap()
        } else {
            prepared
        };
        assert!(s.store.advance_probe_unit_to_analysis(&prepared).unwrap());
    }
    job
}
