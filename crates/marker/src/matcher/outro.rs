use std::time::Instant;

use crate::fingerprint::{ChromaprintEngine, FingerprintEngine};
use crate::types::DetectedOutro;

use super::consensus::find_season_consensus;

pub fn match_episodes_outros(
    episodes: &[(u32, Vec<u32>, i64)],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Vec<DetectedOutro> {
    match_episodes_outros_with(
        &ChromaprintEngine,
        episodes,
        min_duration_secs,
        max_duration_secs,
    )
}

pub fn match_episodes_outros_with(
    engine: &dyn FingerprintEngine,
    episodes: &[(u32, Vec<u32>, i64)],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Vec<DetectedOutro> {
    let started = Instant::now();
    let episode_numbers = episodes.iter().map(|e| e.0).collect::<Vec<_>>();
    let consensus = find_season_consensus(
        &episode_numbers,
        min_duration_secs,
        max_duration_secs,
        |first, second| {
            if crate::adaptive::model::is_constant_or_silent(&episodes[first].1)
                || crate::adaptive::model::is_constant_or_silent(&episodes[second].1)
            {
                tracing::debug!(first, second, "拒绝静音或常量片尾指纹");
                return crate::PairFingerprintDecision::Uncertain("unusable_fingerprint".into());
            }
            crate::PairFingerprintDecision::Candidates(engine.find_common_segments(
                &episodes[first].1,
                &episodes[second].1,
                min_duration_secs,
                max_duration_secs,
            ))
        },
    );
    if consensus.ranges.is_empty() {
        tracing::warn!(
            episodes = episodes.len(),
            pairs_checked = consensus.pairs_checked,
            candidate_pairs = consensus.candidate_pairs,
            candidate_segments = consensus.candidate_segments,
            rejected_episode_support_clusters = consensus.rejected_episode_support_clusters,
            minimum_support = consensus.minimum_support,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "【片头片尾】片尾候选未达到整季一致性门槛"
        );
        return Vec::new();
    }

    let results = consensus
        .ranges
        .iter()
        .map(|range| DetectedOutro {
            episode: episodes[range.episode_index].0,
            outro_start_ms: episodes[range.episode_index].2 + seconds_to_ms(range.start_secs),
            outro_end_ms: episodes[range.episode_index].2 + seconds_to_ms(range.end_secs),
        })
        .collect::<Vec<_>>();
    for marker in &results {
        tracing::debug!(
            episode = marker.episode,
            start_ms = marker.outro_start_ms,
            end_ms = marker.outro_end_ms,
            "【片头片尾】整季共识片尾区间"
        );
    }
    tracing::info!(
        applied = results.len(),
        pairs_checked = consensus.pairs_checked,
        candidate_pairs = consensus.candidate_pairs,
        candidate_segments = consensus.candidate_segments,
        rejected_episode_support_clusters = consensus.rejected_episode_support_clusters,
        consensus_clusters = consensus.consensus_clusters,
        supporting_episodes = consensus.supporting_episodes,
        supporting_pairs = consensus.supporting_pairs,
        minimum_support = consensus.minimum_support,
        median_duration_secs = consensus.median_duration_secs,
        elapsed_ms = started.elapsed().as_millis(),
        "【片头片尾】整季片尾声纹比对完成"
    );
    results
}

fn seconds_to_ms(seconds: f32) -> i64 {
    (seconds * 1000.0) as i64
}
