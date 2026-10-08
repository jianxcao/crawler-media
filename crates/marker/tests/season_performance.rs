use marker::{
    AudioFingerprint, ChromaprintEngine, CommonSegment, FingerprintEngine,
    match_episodes_fingerprints_with,
};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct ComparisonCounter {
    comparisons: AtomicUsize,
}

impl FingerprintEngine for ComparisonCounter {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        unreachable!("cached-season comparison must not read media")
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        self.find_common_segments(first, second, min_duration_secs, max_duration_secs)
            .into_iter()
            .next()
    }

    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Vec<CommonSegment> {
        self.comparisons.fetch_add(1, Ordering::Relaxed);
        ChromaprintEngine.find_common_segments(first, second, min_duration_secs, max_duration_secs)
    }
}

fn cached_sample() -> Vec<u32> {
    (0..1_454)
        .map(|index| {
            let value = index as u32;
            ((500 + value) << 20) | ((value.wrapping_mul(7_919) + 31) & 0x000f_ffff)
        })
        .collect()
}

#[test]
fn eight_cached_episodes_compare_once_per_pair_without_media_reads() {
    let engine = ComparisonCounter {
        comparisons: AtomicUsize::new(0),
    };
    let sample = cached_sample();
    let episodes = (1..=8)
        .map(|episode| (episode, sample.clone()))
        .collect::<Vec<_>>();
    let started = Instant::now();

    let markers = match_episodes_fingerprints_with(&engine, &episodes, 15.0, 240.0);
    let elapsed = started.elapsed();

    assert_eq!(engine.comparisons.load(Ordering::Relaxed), 28);
    assert_eq!(markers.len(), 8);
    eprintln!(
        "cached 8-episode season comparison: {} pairs, {} markers, {:.2}ms, 0 media reads",
        engine.comparisons.load(Ordering::Relaxed),
        markers.len(),
        elapsed.as_secs_f64() * 1000.0
    );
}
