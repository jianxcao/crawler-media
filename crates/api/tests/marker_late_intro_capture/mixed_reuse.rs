use super::*;
use api::fingerprint_job::CapturePolicy;
use api::probe_manager::ProbeManager;
use std::sync::Arc;

fn restart(s: &mut support::ProbeScenario, engine: &Arc<BoundedEngine>) {
    s.store = Arc::new(store::Store::open(s.tmp.path()).unwrap());
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), engine.clone(), engine.clone()).unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
}
fn current_intro_samples(s: &support::ProbeScenario) -> Vec<store::StoredFingerprintSample> {
    let models = s
        .store
        .fingerprint_models_page(
            &s.media_id.to_string(),
            1,
            "intro",
            &api::fingerprint_job::analysis_policy_key(&marker::SamplingPolicy {
                max_windows_per_kind: 3,
                ..Default::default()
            }),
            None,
            3,
        )
        .unwrap();
    let bundle = s
        .store
        .load_fingerprint_model_bundle(&models[0].model_id)
        .unwrap()
        .unwrap();
    let mut samples = bundle.samples.clone();
    samples.sort_by_key(|sample| sample.ledger_id.clone());
    samples
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_positions_reopened_reuse_selects_own_reference_and_freshly_verifies_every_intro() {
    let (mut s, raw) = scenario(3, 330_000, 410_000, None).await;
    s.manager.shutdown_workers().await.unwrap();
    raw.mixed.store(true, std::sync::atomic::Ordering::Relaxed);
    let engine = Arc::new(BoundedEngine(raw));
    set_source_metadata(&s);
    restart(&mut s, &engine);
    assert_eq!(run(&s).await.status, "succeeded");
    let before = current_intro_samples(&s);
    let frames = s
        .ledgers
        .iter()
        .map(|row| {
            s.store
                .current_chapter_frame_task(row.file.file_id)
                .unwrap()
                .unwrap()
                .task
        })
        .collect::<Vec<_>>();
    let reads = engine.0.requests.lock().unwrap().len();
    restart(&mut s, &engine);
    assert_eq!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ),
        3
    );
    let warm = wait(&s).await;
    assert_eq!(warm.status, "succeeded");
    assert_eq!(
        engine.0.requests.lock().unwrap().len(),
        reads,
        "warm verification must use cached captures"
    );
    for (index, row) in s.ledgers.iter().enumerate() {
        let previous = before
            .iter()
            .find(|sample| sample.ledger_id == row.file.file_id.to_string())
            .unwrap();
        let units = s.store.probe_job_units(&warm.id).unwrap();
        let unit = units
            .iter()
            .find(|unit| unit.ledger_id == previous.ledger_id)
            .unwrap();
        let result: serde_json::Value =
            serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
        assert_eq!(result["kinds"]["intro"]["status"], "verified");
        assert_eq!(result["kinds"]["intro"]["retained_previous"], false);
        let plan: serde_json::Value =
            serde_json::from_str(unit.sampling_plan_json.as_deref().unwrap()).unwrap();
        let selected = plan["selected_samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| {
                s.store
                    .fingerprint_sample_by_id(id.as_str().unwrap())
                    .unwrap()
                    .unwrap()
            })
            .filter(|sample| sample.kind == "intro")
            .collect::<Vec<_>>();
        assert_eq!(
            selected,
            vec![previous.clone()],
            "the target must be its own same-source capture, without an unrelated initial window"
        );
        assert_eq!(previous.source_version, row.file.version.source_version);
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            (marker.intro_start_ms, marker.intro_end_ms),
            [
                (Some(330_001), Some(409_999)),
                (Some(30_001), Some(109_999)),
                (Some(180_001), Some(259_999)),
            ][index]
        );
        let metrics: marker::CaptureMetrics = serde_json::from_str(&previous.metrics_json).unwrap();
        assert_eq!(metrics.source_facts, Some(source()));
        assert_eq!(metrics.time_mapping_version, 4);
        assert_eq!(
            s.store
                .current_chapter_frame_task(row.file.file_id)
                .unwrap()
                .unwrap()
                .task,
            frames[index]
        );
    }
    assert!(
        s.store
            .list_fingerprint_attempts_for_job(&warm.id)
            .unwrap()
            .is_empty()
    );
}

// Synthetic mapping4 evidence exercises the same bounded-reference classification
// as the observed media. This fixture is not a producer or real-payload proof test.
struct BoundedEngine(Arc<LateEngine>);
fn source() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        stream_time_base: Some("1/1000".into()),
        codec_name: Some("aac".into()),
        native_sample_rate: Some(44_100),
        audio_channels: Some(2),
        ..support::fixture_source()
    }
}
fn set_source_metadata(s: &support::ProbeScenario) {
    for row in &s.ledgers {
        let mut tracks = s
            .store
            .get_file_meta(&row.file.file_id.to_string())
            .unwrap()
            .unwrap();
        tracks.audio[0].capture_source = Some(source());
        s.store
            .put_file_meta_versioned(
                &row.file.file_id.to_string(),
                &tracks,
                Some(&row.file.version.source_version),
                Some(2_400_000),
            )
            .unwrap();
    }
}
impl marker::FingerprintEngine for BoundedEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> Option<marker::CommonSegment> {
        let _ = (min, max);
        if self.0.start < 0 {
            return None;
        }
        let position = |episode| match episode {
            1 => 330_000,
            3 => 180_000,
            _ => 30_000,
        };
        let (start_a, start_b) = (position(a[2]), position(b[2]));
        if a[0] > start_a || b[0] > start_b || a[1] < start_a + 80_000 || b[1] < start_b + 80_000 {
            return None;
        }
        Some(marker::CommonSegment {
            start1_sec: (start_a - a[0]) as f32 / 1000.,
            end1_sec: (start_a + 80_000 - a[0]) as f32 / 1000.,
            start2_sec: (start_b - b[0]) as f32 / 1000.,
            end2_sec: (start_b + 80_000 - b[0]) as f32 / 1000.,
            duration_sec: 80.,
            score: 0.,
        })
    }
    fn compare_fingerprints(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> marker::PairFingerprintDecision {
        match marker::FingerprintEngine::find_common_segment(self, a, b, min, max) {
            Some(candidate) => marker::PairFingerprintDecision::Candidates(vec![candidate]),
            None => marker::PairFingerprintDecision::NoMatch,
        }
    }
}
impl marker::FingerprintCaptureEngine for BoundedEngine {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let mut capture =
            marker::FingerprintCaptureEngine::capture_window(self.0.as_ref(), request)?;
        let sample_count = request.window.duration_ms() as u64 * 16;
        let decoded_native_samples = request.window.duration_ms() as u64 * 44_100 / 1000;
        let packet_count = decoded_native_samples.div_ceil(1024);
        capture.metrics.time_mapping_version = 4;
        capture.metrics.source_facts = Some(source());
        capture.metrics.sample_count = Some(sample_count);
        capture.metrics.pcm_bytes = sample_count * 2;
        capture.metrics.quantized_mapping = Some(marker::QuantizedPcmMapping {
            origin_min_us: request.window.start_ms * 1000 - 500,
            origin_max_us: request.window.start_ms * 1000 + 500,
            native_sample_rate: 44_100,
            source_tick_us: 1000,
            packet_count,
            block_anchors: 2,
            decoded_native_samples,
            codec_native_samples: packet_count * 1024,
            encoded_payload_sha256: "a".repeat(64),
            proof_version: 1,
        });
        assert!(!marker::boundary::evidence::complete_pcm(&capture));
        Ok(capture)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaced_source_cannot_borrow_its_old_reference_or_publish_an_intro() {
    let (mut s, raw) = scenario(3, 330_000, 410_000, None).await;
    s.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(BoundedEngine(raw));
    set_source_metadata(&s);
    restart(&mut s, &engine);
    assert_eq!(run(&s).await.status, "succeeded");
    let old = s.ledgers[0].clone();
    let tracks = s
        .store
        .get_file_meta(&old.file.file_id.to_string())
        .unwrap()
        .unwrap();
    let old_sample = current_intro_samples(&s)
        .into_iter()
        .find(|sample| sample.ledger_id == old.file.file_id.to_string())
        .unwrap();
    let mut file = old.file.clone();
    std::fs::write(&file.path, "http://127.0.0.1:8080/replaced-source.mkv\n").unwrap();
    file.version.source_version = api::fingerprint_job::current_source_version(&file.path);
    s.store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: file.library_id,
                config_revision: s
                    .store
                    .library_config_revision(&file.library_id.to_string())
                    .unwrap(),
            },
            &[store::library_write::FilePublication {
                file: &file,
                units: &old.units,
                expected_version: Some(&old.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            2000,
        )
        .unwrap();
    s.ledgers[0] = s.store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    s.store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &tracks,
            Some(&s.ledgers[0].file.version.source_version),
            Some(2_400_000),
        )
        .unwrap();
    set_source_metadata(&s);
    let absent = Arc::new(BoundedEngine(Arc::new(LateEngine {
        start: -1,
        end: 0,
        mixed: std::sync::atomic::AtomicBool::new(false),
        fault: std::sync::Mutex::new(None),
        requests: std::sync::Mutex::new(vec![]),
        on_capture: std::sync::Mutex::new(None),
    })));
    restart(&mut s, &absent);
    assert_eq!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ),
        3
    );
    let job = wait(&s).await;
    assert_eq!(job.status, "succeeded");
    let current = &s.ledgers[0];
    assert_ne!(
        current.file.version.source_version,
        old_sample.source_version
    );
    assert!(
        s.store
            .get_file_marker(current.file.file_id, current.units[0])
            .unwrap()
            .is_none()
    );
    let units = s.store.probe_job_units(&job.id).unwrap();
    let unit = units
        .iter()
        .find(|unit| unit.ledger_id == old_sample.ledger_id)
        .unwrap();
    let result: serde_json::Value =
        serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["kinds"]["intro"]["status"], "uncertain");
    assert_eq!(result["kinds"]["intro"]["retained_previous"], false);
    let plan: serde_json::Value =
        serde_json::from_str(unit.sampling_plan_json.as_deref().unwrap()).unwrap();
    for id in plan["selected_samples"].as_array().unwrap() {
        let sample = s
            .store
            .fingerprint_sample_by_id(id.as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(sample.sample_id, old_sample.sample_id);
        assert_eq!(sample.source_version, current.file.version.source_version);
    }
    let target_windows = absent
        .0
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, window)| {
            path == &current.file.path.to_string_lossy() && window.start_ms < 600_000
        })
        .map(|(_, window)| (window.start_ms, window.end_ms))
        .collect::<Vec<_>>();
    assert_eq!(
        target_windows,
        vec![(0, 180_000), (120_000, 420_000), (300_000, 600_000)]
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_file_without_own_mixed_position_reference_starts_with_bounded_fallback() {
    let (mut s, raw) = scenario(3, 330_000, 410_000, None).await;
    s.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(BoundedEngine(raw));
    set_source_metadata(&s);
    restart(&mut s, &engine);
    assert_eq!(run(&s).await.status, "succeeded");
    let next = s
        .store
        .analysis_season_files_page(s.media_id, 1, None, 16)
        .unwrap()
        .into_iter()
        .find(|row| row.units.iter().any(|unit| unit.episode == 4))
        .unwrap();
    s.ledgers = vec![next.clone()];
    set_source_metadata(&s);
    restart(&mut s, &engine);
    assert_eq!(
        s.manager
            .enqueue_marker_refresh_with_policy(vec![unit(&next)], CapturePolicy::ReuseValid),
        1
    );
    let job = wait(&s).await;
    assert_eq!(job.status, "succeeded");
    let marker = s
        .store
        .get_file_marker(next.file.file_id, next.units[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        (marker.intro_start_ms, marker.intro_end_ms),
        (Some(30_001), Some(109_999))
    );
    let requests = engine
        .0
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == &next.file.path.to_string_lossy())
        .map(|(_, window)| (window.start_ms, window.end_ms))
        .collect::<Vec<_>>();
    assert_eq!(requests, vec![(0, 180_000), (2_220_000, 2_400_000)]);
}
