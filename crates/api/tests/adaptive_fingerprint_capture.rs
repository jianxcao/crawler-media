use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;

use api::fingerprint_job::{
    capture_episode_adaptive, capture_profile_key,
    AdaptiveCaptureContext, CaptureGate, CapturePolicy, EpisodeCaptureRequest,
    FingerprintCaptureProfile,
};
use domain::LedgerRow;
use marker::adaptive::{SamplingPolicy, TemplateContext};
use marker::fingerprint::capture_types::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CapturedFingerprint,
    FingerprintCaptureEngine, SampleWindow,
};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use store::Store;

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
        self.captured_windows.lock().unwrap().push(request.window.clone());
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

fn dummy_row(ledger_id: &str, ep: u32) -> LedgerRow {
    let dummy_path = std::env::temp_dir().join(format!("dummy_{ledger_id}.mkv"));
    let _ = std::fs::write(&dummy_path, b"dummy");
    LedgerRow {
        id: domain::LedgerId::new(),
        media_id: domain::MediaId::new(),
        path: dummy_path.to_string_lossy().to_string(),
        season: Some(1),
        episode: Some(ep),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
    }
}

fn default_profile() -> FingerprintCaptureProfile {
    FingerprintCaptureProfile {
        algorithm_version: 1,
        preset: "preset_test2".into(),
        pcm_sample_rate: 16000,
        pcm_channels: 1,
        pcm_format: "s16le".into(),
        audio_stream_index: None,
        audio_selection_version: 1,
        time_mapping_version: 1,
    }
}

#[tokio::test]
async fn reuse_valid_does_not_read_media() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(temp.path()).unwrap()));
    let engine = Arc::new(MockEngine::new(0));
    let ctx = AdaptiveCaptureContext {
        store: store.clone(),
        matcher: engine.clone(),
        capture: engine.clone(),
        gate: Arc::new(NoopGate),
        timings: None,
    };

    let profile = default_profile();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    let row = dummy_row("l-1", 1);
    store.lock().insert_ledger(&row).unwrap();
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
    let store = Arc::new(Mutex::new(Store::open(temp.path()).unwrap()));
    let engine = Arc::new(MockEngine::new(0));
    let ctx = AdaptiveCaptureContext {
        store: store.clone(),
        matcher: engine.clone(),
        capture: engine.clone(),
        gate: Arc::new(NoopGate),
        timings: None,
    };

    let profile = default_profile();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    let row = dummy_row("l-1", 1);
    store.lock().insert_ledger(&row).unwrap();
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
    let store = Arc::new(Mutex::new(Store::open(temp.path()).unwrap()));
    // Fails 5 times -> should exhaust 4 attempts budget and fail
    let engine = Arc::new(MockEngine::new(5));
    let ctx = AdaptiveCaptureContext {
        store: store.clone(),
        matcher: engine.clone(),
        capture: engine.clone(),
        gate: Arc::new(NoopGate),
        timings: None,
    };

    let profile = default_profile();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    let row = dummy_row("l-exhaust", 1);
    store.lock().insert_ledger(&row).unwrap();
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

    let store_guard = store.lock();
    let attempts = store_guard
        .list_fingerprint_attempts_for_job("job-exhaust")
        .unwrap_or_default();
    assert_eq!(attempts.len(), 4);
    assert!(attempts.iter().all(|a| a.status == "failed"));
}
