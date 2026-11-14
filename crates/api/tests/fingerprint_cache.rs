use api::fingerprint_job::{
    capture_or_reuse_fingerprints_from_source as capture_or_reuse_fingerprints,
    current_source_version,
};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use store::Store;

struct CountingEngine {
    calls: AtomicUsize,
    windows: Mutex<Vec<(u32, u32)>>,
}

impl FingerprintEngine for CountingEngine {
    fn extract_at(
        &self,
        _path: &Path,
        start_secs: u32,
        duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<AudioFingerprint, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.windows
            .lock()
            .unwrap()
            .push((start_secs, duration_secs));
        Ok(vec![start_secs, duration_secs, 7])
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

impl marker::FingerprintCaptureEngine for CountingEngine {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let words = self
            .extract_at(
                &request.path,
                (request.window.start_ms / 1000) as u32,
                (request.window.duration_ms() / 1000) as u32,
                &marker::WorkControl::default(),
            )
            .unwrap();
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words,
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(0),
                source_facts: Some(facts()),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                ..Default::default()
            },
        })
    }
}

#[tokio::test]
async fn unchanged_season_refresh_reuses_both_samples_and_changed_source_recaptures() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let stream = temp.path().join("episode.strm");
    std::fs::write(&stream, "https://cdn.example/episode-a.mkv\n").unwrap();
    let store = Store::open(&data).unwrap();
    let engine = Arc::new(CountingEngine {
        calls: AtomicUsize::new(0),
        windows: Mutex::new(Vec::new()),
    });
    let source_version = current_source_version(&stream);

    let first = capture_or_reuse_fingerprints(
        &stream,
        &source_version,
        180,
        Some(2_700_000),
        None,
        true,
        engine.clone(),
        &marker::WorkControl::default(),
        Some(facts()),
    )
    .await
    .unwrap();
    assert!(!first.intro_cache_hit);
    assert!(!first.outro_cache_hit);
    assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    assert_eq!(*engine.windows.lock().unwrap(), [(0, 180), (2_520, 180)]);
    store
        .put_fingerprint_cache("episode", &first.cache)
        .unwrap();

    let started = Instant::now();
    let persisted = Store::open(&data)
        .unwrap()
        .get_fingerprint_cache("episode")
        .unwrap();
    let reused = capture_or_reuse_fingerprints(
        &stream,
        &source_version,
        180,
        Some(2_700_000),
        persisted,
        true,
        engine.clone(),
        &marker::WorkControl::default(),
        Some(facts()),
    )
    .await
    .unwrap();
    let warm_elapsed = started.elapsed();
    assert!(reused.intro_cache_hit);
    assert!(reused.outro_cache_hit);
    assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    eprintln!(
        "unchanged episode refresh reused in {}µs with 0 audio reads",
        warm_elapsed.as_micros()
    );

    std::fs::write(&stream, "https://cdn.example/episode-b.mkv\n").unwrap();
    let changed_version = current_source_version(&stream);
    let stale = capture_or_reuse_fingerprints(
        &stream,
        &changed_version,
        180,
        Some(2_700_000),
        Some(reused.cache),
        true,
        engine.clone(),
        &marker::WorkControl::default(),
        Some(facts()),
    )
    .await
    .unwrap();
    assert!(!stale.intro_cache_hit);
    assert!(!stale.outro_cache_hit);
    assert_eq!(engine.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn explicit_recapture_bypasses_an_unchanged_cache() {
    let temp = tempfile::tempdir().unwrap();
    let stream = temp.path().join("episode.strm");
    std::fs::write(&stream, "https://cdn.example/episode.mkv\n").unwrap();
    let engine = Arc::new(CountingEngine {
        calls: AtomicUsize::new(0),
        windows: Mutex::new(Vec::new()),
    });
    let source_version = current_source_version(&stream);
    let first = capture_or_reuse_fingerprints(
        &stream,
        &source_version,
        180,
        Some(2_700_000),
        None,
        true,
        engine.clone(),
        &marker::WorkControl::default(),
        Some(facts()),
    )
    .await
    .unwrap();
    let recaptured = capture_or_reuse_fingerprints(
        &stream,
        &source_version,
        180,
        Some(2_700_000),
        Some(first.cache),
        false,
        engine.clone(),
        &marker::WorkControl::default(),
        Some(facts()),
    )
    .await
    .unwrap();

    assert!(!recaptured.intro_cache_hit);
    assert!(!recaptured.outro_cache_hit);
    assert_eq!(engine.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn fractional_duration_preserves_the_capture_timeline_and_pcm_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Arc::new(CountingEngine {
        calls: AtomicUsize::new(0),
        windows: Mutex::new(Vec::new()),
    });
    let result = capture_or_reuse_fingerprints(
        &temp.path().join("episode.mkv"),
        "source",
        180,
        Some(1_800_750),
        None,
        true,
        engine,
        &marker::WorkControl::default(),
        Some(facts()),
    )
    .await
    .unwrap();
    let outro = result.cache.outro.unwrap();
    assert_eq!(outro.window.start_ms, 1_620_750);
    assert_eq!(outro.window.end_ms, 1_800_750);
    assert_eq!(outro.pcm_duration_ms, Some(180_000));
    let mapped = marker::boundary::normalize_interval(
        &outro.window,
        marker::boundary::Interval {
            start_ms: 5_000,
            end_ms: 65_000,
        },
    )
    .unwrap();
    assert_eq!(mapped.start_ms, 1_625_750);
    assert_eq!(outro.metrics.time_mapping_version, 3);
}

#[test]
fn every_boundary_policy_field_participates_in_analysis_identity() {
    use api::fingerprint_job::analysis_policy_key;
    let policy = marker::SamplingPolicy::default();
    let original = analysis_policy_key(&policy);
    let variants: [fn(&mut marker::boundary::BoundaryPolicy); 8] = [
        |p| p.pcm_frame_ms += 1,
        |p| p.silence_dbfs += 1.0,
        |p| p.silence_duration_ms += 1,
        |p| p.transition_db += 1.0,
        |p| p.transition_window_ms += 1,
        |p| p.context_ms += 1,
        |p| p.agreement_ms += 1,
        |p| p.min_guard_ms += 1,
    ];
    for mutate in variants {
        let mut changed = policy.clone();
        mutate(&mut changed.boundary);
        assert_ne!(original, analysis_policy_key(&changed));
    }
}

fn facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "test_source_header".into(),
    }
}
