use api::fingerprint_job::{capture_or_reuse_fingerprints, current_source_version};
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
    )
    .await
    .unwrap();

    assert!(!recaptured.intro_cache_hit);
    assert!(!recaptured.outro_cache_hit);
    assert_eq!(engine.calls.load(Ordering::SeqCst), 4);
}
