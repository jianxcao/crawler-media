use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::fingerprint_job::{
    AdaptiveCaptureContext, CaptureGate, CapturePolicy, EpisodeCaptureRequest,
    FingerprintCaptureProfile, capture_episode_adaptive, capture_profile_key,
};
use domain::{FileCoverage, UnitKey};
use marker::adaptive::{SamplingPolicy, TemplateContext};
use marker::fingerprint::capture_types::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CapturedFingerprint,
    FingerprintCaptureEngine, SampleWindow,
};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct NoopGate;

impl CaptureGate for NoopGate {
    fn wait_before_capture(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

struct MockEngine {
    capture_calls: AtomicUsize,
    fail_attempts: usize,
    captured_windows: std::sync::Mutex<Vec<SampleWindow>>,
}

impl MockEngine {
    fn new(fail_attempts: usize) -> Self {
        Self {
            capture_calls: AtomicUsize::new(0),
            fail_attempts,
            captured_windows: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl FingerprintCaptureEngine for MockEngine {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let call_idx = self.capture_calls.fetch_add(1, Ordering::SeqCst);
        self.captured_windows
            .lock()
            .unwrap()
            .push(request.window.clone());
        if call_idx < self.fail_attempts {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Io,
                message: "simulated io error".into(),
                metrics: CaptureMetrics {
                    elapsed_ms: 10,
                    pcm_bytes: 0,
                    ..Default::default()
                },
            });
        }
        let dur_ms = request.window.duration_ms();
        // Return dummy words
        let num_words = (dur_ms / 128).max(1) as usize;
        let words = (0..num_words).map(|i| (i as u32) + 1).collect();
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words,
            pcm_duration_ms: Some(dur_ms),
            metrics: CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(0),
                source_facts: Some(source_facts()),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                elapsed_ms: 20,
                pcm_bytes: (num_words * 4) as u64,
                ..Default::default()
            },
        })
    }
}

impl FingerprintEngine for MockEngine {
    fn extract_at(
        &self,
        _path: &std::path::Path,
        _start_secs: u32,
        _duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<AudioFingerprint, String> {
        Ok(vec![1, 2, 3])
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        None
    }
}

fn dummy_row(store: &Store, ledger_id: &str, ep: u32) -> FileCoverage {
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Tv,
            title: format!("Fixture {ledger_id}"),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let dummy_path = store
        .library_root(domain::MediaKind::Tv)
        .unwrap()
        .join(format!("dummy_{ledger_id}.mkv"));
    std::fs::write(&dummy_path, b"dummy").unwrap();
    let spec = FixtureFile {
        media_id,
        path: dummy_path.to_string_lossy().to_string(),
        season: Some(1),
        episode: Some(ep),
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        ..FixtureFile::default()
    };
    // The published file fact — and with it the `file_id` the pipeline keys on —
    // is only known after publication, so read it back instead of inventing one.
    let file = publish_fixture_file(store, &spec).unwrap();
    store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &library::Tracks {
                video: Some(Default::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    capture_source: Some(source_facts()),
                    is_default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(&file.version.source_version),
            Some(1_800_000),
        )
        .unwrap();
    FileCoverage {
        path_key: String::new(),
        file,
        units: vec![UnitKey::new(1, ep as i32)],
    }
}

fn default_profile() -> FingerprintCaptureProfile {
    FingerprintCaptureProfile {
        audio_stream_index: Some(0),
        ..Default::default()
    }
}

fn source_facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture_header".into(),
    }
}

#[tokio::test]
async fn reuse_valid_does_not_read_media() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(temp.path()).unwrap());
    let engine = Arc::new(MockEngine::new(0));
    let ctx = AdaptiveCaptureContext {
        store: api::probe_manager::ProbeStoreAccess::new(store.clone()),
        matcher: Arc::new(marker::matcher::session::SeasonMatchSession::new(
            engine.clone(),
            10_000,
            32 * 1024 * 1024,
        )),
        capture: engine.clone(),
        gate: Arc::new(NoopGate),
        timings: None,
    };

    let profile = default_profile();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    let row = dummy_row(store.as_ref(), "l-1", 1);
    let req = EpisodeCaptureRequest {
        job_id: "job-1".into(),
        row,
        source_version: "src-v1".into(),
        media_duration_ms: Some(1_800_000),
        audio_stream_index: None,
        capture_profile_key: profile_key.clone(),
        policy: policy.clone(),
        capture_policy: CapturePolicy::ReuseValid,
        templates: TemplateContext::default(),
        cost_summary: marker::adaptive::SourceCostSummary::default(),
    };

    // First time: will capture
    let _res1 = capture_episode_adaptive(&ctx, &req).await.unwrap();
    assert_eq!(engine.capture_calls.load(Ordering::SeqCst), 2); // intro + outro

    // Second time with ReuseValid: should not capture again
    let _res2 = capture_episode_adaptive(&ctx, &req).await.unwrap();
    assert_eq!(engine.capture_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn recapture_requires_new_job_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(temp.path()).unwrap());
    let engine = Arc::new(MockEngine::new(0));
    let ctx = AdaptiveCaptureContext {
        store: api::probe_manager::ProbeStoreAccess::new(store.clone()),
        matcher: Arc::new(marker::matcher::session::SeasonMatchSession::new(
            engine.clone(),
            10_000,
            32 * 1024 * 1024,
        )),
        capture: engine.clone(),
        gate: Arc::new(NoopGate),
        timings: None,
    };

    let profile = default_profile();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    let row = dummy_row(store.as_ref(), "l-1", 1);
    let req1 = EpisodeCaptureRequest {
        job_id: "job-1".into(),
        row,
        source_version: "src-v1".into(),
        media_duration_ms: Some(1_800_000),
        audio_stream_index: None,
        capture_profile_key: profile_key.clone(),
        policy: policy.clone(),
        capture_policy: CapturePolicy::ReuseValid,
        templates: TemplateContext::default(),
        cost_summary: marker::adaptive::SourceCostSummary::default(),
    };

    capture_episode_adaptive(&ctx, &req1).await.unwrap();
    assert_eq!(engine.capture_calls.load(Ordering::SeqCst), 2);

    // Now Recapture under job-2: must re-capture despite existing samples
    let req2 = EpisodeCaptureRequest {
        job_id: "job-2".into(),
        capture_policy: CapturePolicy::Recapture,
        ..req1
    };
    capture_episode_adaptive(&ctx, &req2).await.unwrap();
    assert_eq!(engine.capture_calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn retries_and_fallback_share_four_attempt_budget() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(temp.path()).unwrap());
    // Fails 5 times -> should exhaust 4 attempts budget and fail
    let engine = Arc::new(MockEngine::new(5));
    let ctx = AdaptiveCaptureContext {
        store: api::probe_manager::ProbeStoreAccess::new(store.clone()),
        matcher: Arc::new(marker::matcher::session::SeasonMatchSession::new(
            engine.clone(),
            10_000,
            32 * 1024 * 1024,
        )),
        capture: engine.clone(),
        gate: Arc::new(NoopGate),
        timings: None,
    };

    let profile = default_profile();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    let row = dummy_row(store.as_ref(), "l-exhaust", 1);
    let req = EpisodeCaptureRequest {
        job_id: "job-exhaust".into(),
        row,
        source_version: "src-v1".into(),
        media_duration_ms: Some(1_800_000),
        audio_stream_index: None,
        capture_profile_key: profile_key.clone(),
        policy: policy.clone(),
        capture_policy: CapturePolicy::ReuseValid,
        templates: TemplateContext::default(),
        cost_summary: marker::adaptive::SourceCostSummary::default(),
    };

    let res = capture_episode_adaptive(&ctx, &req).await;
    assert!(res.is_err());

    let store_guard = store.as_ref();
    let attempts = store_guard
        .list_fingerprint_attempts_for_job("job-exhaust")
        .unwrap_or_default();
    assert_eq!(attempts.len(), 4);
    assert!(attempts.iter().all(|a| a.status == "failed"));
}

struct TailOnly(MockEngine);
impl FingerprintCaptureEngine for TailOnly {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        if request.window.start_ms == 0 {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Io,
                message: "successful head cannot be reread".into(),
                metrics: Default::default(),
            });
        }
        self.0.capture_window(request)
    }
}

#[tokio::test]
async fn recapture_retry_uses_its_own_head_and_new_job_cannot_reuse_it() {
    use api::fingerprint_job::adaptive::capture_or_reuse_segment_sample;
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(temp.path()).unwrap());
    let engine = Arc::new(MockEngine::new(0));
    let mut ctx = AdaptiveCaptureContext {
        store: api::probe_manager::ProbeStoreAccess::new(store.clone()),
        matcher: Arc::new(marker::matcher::session::SeasonMatchSession::new(
            engine.clone(),
            10_000,
            32 * 1024 * 1024,
        )),
        capture: engine,
        gate: Arc::new(NoopGate),
        timings: None,
    };
    let mut request = EpisodeCaptureRequest {
        job_id: "same-job".into(),
        row: dummy_row(&store, "head-only", 1),
        source_version: "src-v1".into(),
        media_duration_ms: Some(1_800_000),
        audio_stream_index: Some(0),
        capture_profile_key: capture_profile_key(&default_profile()),
        policy: SamplingPolicy::default(),
        capture_policy: CapturePolicy::Recapture,
        templates: TemplateContext::default(),
        cost_summary: Default::default(),
    };
    let mut attempts = 0;
    let head = capture_or_reuse_segment_sample(
        &ctx,
        &request,
        marker::SegmentKind::Intro,
        &SampleWindow::new(0, 180_000),
        "head",
        &mut attempts,
        4,
    )
    .await
    .unwrap();
    ctx.capture = Arc::new(TailOnly(MockEngine::new(0)));
    let recovered = capture_episode_adaptive(&ctx, &request).await.unwrap();
    assert_eq!(recovered.intro_evidence.unwrap().sample_id, head.sample_id);
    assert!(recovered.outro_evidence.is_some());
    request.job_id = "new-explicit-recapture".into();
    assert!(capture_episode_adaptive(&ctx, &request).await.is_err());
}

struct NoopFetcher;

impl indexer::Fetcher for NoopFetcher {
    fn fetch(&self, _req: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Ok(String::new())
    }
}

fn tv_probe_unit(row: FileCoverage) -> api::probe_manager::ProbeUnit {
    api::probe_manager::ProbeUnit {
        row,
        kind: domain::MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }
}

struct StubTracks;

impl api::probe_manager::TracksProbe for StubTracks {
    fn probe(
        &self,
        _path: &std::path::Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("hevc".into()),
                    width: Some(1920),
                    height: Some(1080),
                    bit_depth: Some(8),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    codec: Some("aac".into()),
                    channels: Some(2),
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: Vec::new(),
            },
            Some(1_800_000),
        ))
    }
}

async fn wait_unit_idle(state: &api::ApiState, ledger_id: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if state
            .store()
            .active_probe_unit_for_ledger(ledger_id)
            .unwrap()
            .is_none()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "unit {ledger_id} did not finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn build_probe_state(
    data_dir: std::path::PathBuf,
    stage_dir: std::path::PathBuf,
    engine: Arc<MockEngine>,
) -> api::ApiState {
    let state = std::thread::spawn(move || {
        api::ApiState::new(
            Store::open(&data_dir).unwrap(),
            "test-secret".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(NoopFetcher),
            Arc::new(downloader::MemoryDownloader::new(stage_dir)),
        )
        .unwrap()
    })
    .join()
    .unwrap()
    .with_probe_engines(engine.clone(), engine, Arc::new(StubTracks))
    .unwrap();
    assert!(state.probe.try_start_workers());
    state
}

async fn save_scrape_sampling(app: &axum::Router, mode: &str, secs: u32) {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/scrape")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-secret")
        .body(Body::from(
            serde_json::json!({
                "setting": {
                    "fingerprint_sampling_mode": mode,
                    "fingerprint_duration_secs": secs,
                }
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn adaptive_mode_with_custom_duration_captures_180s_and_switches_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let tv_root = data.join("library/tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    let tv_lib = store
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap();
    store
        .set_library_intro_settings(&tv_lib.id, true, true)
        .unwrap();
    let ep1 = dummy_row(&store, "adapt-ep1", 1);
    let ep2 = dummy_row(&store, "adapt-ep2", 2);
    drop(store);

    let engine = Arc::new(MockEngine::new(0));
    let state = build_probe_state(data.clone(), tmp.path().join("stage"), engine.clone());
    let app = api::router(state.clone());

    // 1. Save adaptive + 90s through settings API: captures 180s initial window without error.
    save_scrape_sampling(&app, "adaptive", 90).await;
    state
        .probe
        .enqueue_marker_refresh(vec![tv_probe_unit(ep1.clone())]);
    wait_unit_idle(&state, &ep1.file.file_id.to_string()).await;
    let adaptive_windows = engine.captured_windows.lock().unwrap().clone();
    assert!(
        adaptive_windows
            .iter()
            .any(|w| w.start_ms == 0 && w.end_ms == 180_000),
        "adaptive mode must use 180s initial window even when 90s is saved: {adaptive_windows:?}"
    );
    let ep1_job = state
        .store()
        .latest_probe_job_for_scope(&format!("marker-refresh:{}:1", ep1.file.media_id))
        .unwrap()
        .expect("ep1 marker_refresh job");
    assert_eq!(ep1_job.status, "succeeded");
    assert_eq!(ep1_job.failed, 0);

    // 2. Switch to full_window (still 90s): captures 90s window.
    engine.captured_windows.lock().unwrap().clear();
    save_scrape_sampling(&app, "full_window", 90).await;
    state
        .probe
        .enqueue_marker_refresh(vec![tv_probe_unit(ep2.clone())]);
    wait_unit_idle(&state, &ep2.file.file_id.to_string()).await;
    let full_windows = engine.captured_windows.lock().unwrap().clone();
    assert!(
        full_windows
            .iter()
            .any(|w| w.start_ms == 0 && w.end_ms == 90_000),
        "full_window mode must honor configured 90s duration: {full_windows:?}"
    );
    let ep2_job = state
        .store()
        .latest_probe_job_for_scope(&format!("marker-refresh:{}:1", ep2.file.media_id))
        .unwrap()
        .expect("ep2 marker_refresh job");
    assert_eq!(ep2_job.status, "succeeded");

    // 3. Restart with an already-pinned adaptive plan while settings are full_window 90s.
    let ep3 = dummy_row(&state.store(), "adapt-ep3", 3);
    let ep3_id = ep3.file.file_id.to_string();
    let pinned_job = "restart-pinned-adaptive";
    state
        .store()
        .create_probe_job(
            pinned_job,
            "marker_refresh",
            &ep3.file.media_id.to_string(),
            Some(1),
            &format!("marker-refresh:{}:1", ep3.file.media_id),
            &[store::ProbeJobUnitSpec {
                ledger_id: &ep3_id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: false,
            }],
        )
        .unwrap();
    let pinned_plan = serde_json::json!({
        "searches": [],
        "selected_samples": [],
        "policy": marker::SamplingPolicy::default(),
        "sampling_mode": "adaptive"
    });
    state
        .store()
        .pin_probe_job_sampling_plan(pinned_job, &pinned_plan.to_string())
        .unwrap();
    state.probe.shutdown_workers().await.unwrap();

    let restarted_engine = Arc::new(MockEngine::new(0));
    let restarted = build_probe_state(data, tmp.path().join("stage2"), restarted_engine.clone());
    wait_unit_idle(&restarted, &ep3_id).await;
    let restarted_windows = restarted_engine.captured_windows.lock().unwrap().clone();
    assert!(
        restarted_windows
            .iter()
            .any(|w| w.start_ms == 0 && w.end_ms == 180_000),
        "restarted job must reuse persisted adaptive 180s plan: {restarted_windows:?}"
    );
    restarted.probe.shutdown_workers().await.unwrap();
}
