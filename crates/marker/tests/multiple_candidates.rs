use marker::{
    AudioFingerprint, CommonSegment, FingerprintEngine, match_episodes_fingerprints_with,
};
use std::{collections::HashMap, path::Path};

struct MultiCandidateEngine {
    candidates: HashMap<(u32, u32), Vec<CommonSegment>>,
}

impl FingerprintEngine for MultiCandidateEngine {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        unreachable!("this matcher only compares cached fingerprints")
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
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Vec<CommonSegment> {
        self.candidates
            .get(&(*first.first().unwrap(), *second.first().unwrap()))
            .cloned()
            .unwrap_or_default()
    }
}

fn interval(start1: f32, start2: f32, duration: f32) -> CommonSegment {
    CommonSegment {
        start1_sec: start1,
        end1_sec: start1 + duration,
        start2_sec: start2,
        end2_sec: start2 + duration,
        duration_sec: duration,
        score: 0.5,
    }
}

#[test]
fn season_consensus_uses_a_shorter_candidate_recurring_across_more_episodes() {
    let mut candidates = HashMap::new();
    candidates.insert(
        (1, 2),
        vec![interval(100.0, 300.0, 120.0), interval(5.0, 30.0, 20.0)],
    );
    candidates.insert((1, 3), vec![interval(5.0, 60.0, 20.0)]);
    candidates.insert((2, 3), vec![interval(30.0, 60.0, 20.0)]);
    let engine = MultiCandidateEngine { candidates };

    let markers = match_episodes_fingerprints_with(
        &engine,
        &[(1, vec![1]), (2, vec![2]), (3, vec![3])],
        15.0,
        240.0,
    );

    assert_eq!(markers.len(), 3);
    assert_eq!(
        markers
            .iter()
            .map(|marker| (marker.episode, marker.intro_start_ms, marker.intro_end_ms))
            .collect::<Vec<_>>(),
        [(1, 5_000, 25_000), (2, 30_000, 50_000), (3, 60_000, 80_000)]
    );
}
