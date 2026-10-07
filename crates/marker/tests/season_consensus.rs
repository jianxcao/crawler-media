use marker::{
    AudioFingerprint, CommonSegment, FingerprintEngine, match_episodes_fingerprints_with,
    match_episodes_outros_with,
};
use std::{collections::HashMap, path::Path};

struct PairwiseEngine {
    matches: HashMap<(u32, u32), CommonSegment>,
}

impl FingerprintEngine for PairwiseEngine {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        Ok(Vec::new())
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        let key = (*first.first()?, *second.first()?);
        self.matches.get(&key).cloned()
    }
}

#[test]
fn season_consensus_rejects_an_isolated_long_match_in_favor_of_the_recurring_intro() {
    let mut matches = HashMap::new();
    matches.insert(
        (1, 2),
        CommonSegment {
            start1_sec: 40.0,
            end1_sec: 220.0,
            start2_sec: 110.0,
            end2_sec: 290.0,
            duration_sec: 180.0,
            score: 0.2,
        },
    );
    for first in 3..=34 {
        for second in first + 1..=34 {
            let first_start = 30.0 + first as f32;
            let second_start = 30.0 + second as f32;
            matches.insert(
                (first, second),
                CommonSegment {
                    start1_sec: first_start,
                    end1_sec: first_start + 60.0,
                    start2_sec: second_start,
                    end2_sec: second_start + 60.0,
                    duration_sec: 60.0,
                    score: 0.5,
                },
            );
        }
    }
    let engine = PairwiseEngine { matches };
    let episodes = (1..=34)
        .map(|episode| (episode, vec![episode]))
        .collect::<Vec<_>>();

    let detected = match_episodes_fingerprints_with(&engine, &episodes, 15.0, 240.0);

    let matched_episodes = detected
        .iter()
        .map(|marker| marker.episode)
        .collect::<Vec<_>>();
    assert_eq!(matched_episodes, (3..=34).collect::<Vec<_>>());
}

#[test]
fn season_consensus_keeps_a_two_pair_intro_variant_supported_by_three_episodes() {
    let mut matches = HashMap::new();
    for first in 2..=15 {
        for second in first + 1..=15 {
            matches.insert(
                (first, second),
                CommonSegment {
                    start1_sec: 0.0,
                    end1_sec: 100.0,
                    start2_sec: 0.0,
                    end2_sec: 100.0,
                    duration_sec: 100.0,
                    score: 0.5,
                },
            );
        }
    }
    for second in [2, 3] {
        matches.insert(
            (1, second),
            CommonSegment {
                start1_sec: 12.0,
                end1_sec: 35.0,
                start2_sec: 102.0,
                end2_sec: 125.0,
                duration_sec: 23.0,
                score: 2.5,
            },
        );
    }
    let engine = PairwiseEngine { matches };
    let episodes = (1..=15)
        .map(|episode| (episode, vec![episode]))
        .collect::<Vec<_>>();

    let detected = match_episodes_fingerprints_with(&engine, &episodes, 15.0, 240.0);

    assert_eq!(detected.len(), 15);
    let first_episode = detected
        .iter()
        .find(|marker| marker.episode == 1)
        .expect("the two independent matches support episode one");
    assert_eq!(first_episode.intro_start_ms, 12_000);
    assert_eq!(first_episode.intro_end_ms, 35_000);
}

#[test]
fn season_consensus_keeps_a_three_pair_intro_variant_in_a_large_season() {
    let mut matches = HashMap::new();
    for (first, second, start1, start2) in [
        (1, 2, 12.0, 102.0),
        (1, 3, 12.0, 102.0),
        (2, 3, 102.0, 102.0),
    ] {
        matches.insert(
            (first, second),
            CommonSegment {
                start1_sec: start1,
                end1_sec: start1 + 23.0,
                start2_sec: start2,
                end2_sec: start2 + 23.0,
                duration_sec: 23.0,
                score: 0.5,
            },
        );
    }
    let engine = PairwiseEngine { matches };
    let episodes = (1..=15)
        .map(|episode| (episode, vec![episode]))
        .collect::<Vec<_>>();

    let detected = match_episodes_fingerprints_with(&engine, &episodes, 15.0, 240.0);

    assert_eq!(
        detected
            .iter()
            .map(|marker| marker.episode)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn season_consensus_keeps_two_recurring_intro_variants() {
    let mut matches = HashMap::new();
    for group in [1..=5, 6..=10] {
        let last = *group.end();
        for first in group.clone() {
            for second in first + 1..=last {
                let base = if first <= 5 { 30.0 } else { 100.0 };
                let start1 = base + first as f32;
                let start2 = base + second as f32;
                matches.insert(
                    (first, second),
                    CommonSegment {
                        start1_sec: start1,
                        end1_sec: start1 + 55.0,
                        start2_sec: start2,
                        end2_sec: start2 + 55.0,
                        duration_sec: 55.0,
                        score: 0.5,
                    },
                );
            }
        }
    }
    let engine = PairwiseEngine { matches };
    let episodes = (1..=10)
        .map(|episode| (episode, vec![episode]))
        .collect::<Vec<_>>();

    let detected = match_episodes_fingerprints_with(&engine, &episodes, 15.0, 240.0);

    assert_eq!(detected.len(), 10);
    for marker in &detected[..5] {
        assert_eq!(marker.intro_start_ms, (30 + marker.episode as i64) * 1000);
    }
    for marker in &detected[5..] {
        assert_eq!(marker.intro_start_ms, (100 + marker.episode as i64) * 1000);
    }
}

#[test]
fn season_consensus_preserves_a_supported_longer_variant_nested_in_the_majority_match() {
    let mut matches = HashMap::new();
    for first in 1..=10 {
        for second in first + 1..=10 {
            let start1 = 10.0 + first as f32;
            let start2 = 10.0 + second as f32;
            let duration = if second <= 8 { 70.0 } else { 50.0 };
            matches.insert(
                (first, second),
                CommonSegment {
                    start1_sec: start1,
                    end1_sec: start1 + duration,
                    start2_sec: start2,
                    end2_sec: start2 + duration,
                    duration_sec: duration,
                    score: 0.5,
                },
            );
        }
    }
    let engine = PairwiseEngine { matches };
    let episodes = (1..=10)
        .map(|episode| (episode, vec![episode]))
        .collect::<Vec<_>>();

    let detected = match_episodes_fingerprints_with(&engine, &episodes, 15.0, 240.0);

    assert_eq!(detected.len(), 10);
    for marker in &detected[..8] {
        assert_eq!(marker.intro_end_ms - marker.intro_start_ms, 70_000);
    }
    for marker in &detected[8..] {
        assert_eq!(marker.intro_end_ms - marker.intro_start_ms, 50_000);
    }
}

#[test]
fn season_consensus_preserves_outro_offsets_for_two_recurring_variants() {
    let mut matches = HashMap::new();
    for group in [1..=3, 4..=6] {
        let last = *group.end();
        for first in group.clone() {
            for second in first + 1..=last {
                let base = if first <= 3 { 30.0 } else { 100.0 };
                let start1 = base + first as f32;
                let start2 = base + second as f32;
                matches.insert(
                    (first, second),
                    CommonSegment {
                        start1_sec: start1,
                        end1_sec: start1 + 55.0,
                        start2_sec: start2,
                        end2_sec: start2 + 55.0,
                        duration_sec: 55.0,
                        score: 0.5,
                    },
                );
            }
        }
    }
    let engine = PairwiseEngine { matches };
    let episodes = (1..=6)
        .map(|episode| (episode, vec![episode], 600_000 + i64::from(episode) * 1000))
        .collect::<Vec<_>>();

    let detected = match_episodes_outros_with(&engine, &episodes, 15.0, 240.0);

    assert_eq!(detected.len(), 6);
    for marker in &detected[..3] {
        assert_eq!(
            marker.outro_start_ms,
            600_000 + i64::from(marker.episode) * 1000 + (30 + i64::from(marker.episode)) * 1000
        );
    }
    for marker in &detected[3..] {
        assert_eq!(
            marker.outro_start_ms,
            600_000 + i64::from(marker.episode) * 1000 + (100 + i64::from(marker.episode)) * 1000
        );
    }
}
