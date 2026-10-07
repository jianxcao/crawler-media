use std::time::Instant;

use crate::fingerprint::{ChromaprintEngine, FingerprintEngine};
use crate::types::DetectedOutro;

use super::common::best_pair;

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
    if episodes.len() < 2 {
        return Vec::new();
    }
    let started = Instant::now();
    let (pairs_checked, best) = best_pair(episodes.len(), |first, second| {
        engine.find_common_segment(
            &episodes[first].1,
            &episodes[second].1,
            min_duration_secs,
            max_duration_secs,
        )
    });
    let Some((first, second, common)) = best else {
        tracing::warn!(
            episodes = episodes.len(),
            pairs_checked,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "【片头片尾】所有配对均未发现公共片尾音频片段"
        );
        return Vec::new();
    };

    let mut results = vec![
        DetectedOutro {
            episode: episodes[first].0,
            outro_start_ms: episodes[first].2 + seconds_to_ms(common.start1_sec),
            outro_end_ms: episodes[first].2 + seconds_to_ms(common.end1_sec),
        },
        DetectedOutro {
            episode: episodes[second].0,
            outro_start_ms: episodes[second].2 + seconds_to_ms(common.start2_sec),
            outro_end_ms: episodes[second].2 + seconds_to_ms(common.end2_sec),
        },
    ];
    for (index, (episode, fingerprint, offset_ms)) in episodes.iter().enumerate() {
        if index == first || index == second {
            continue;
        }
        if let Some(found) = engine.find_common_segment(
            &episodes[first].1,
            fingerprint,
            min_duration_secs,
            max_duration_secs,
        ) {
            tracing::debug!(
                episode,
                start_ms = offset_ms + seconds_to_ms(found.start2_sec),
                end_ms = offset_ms + seconds_to_ms(found.end2_sec),
                matched_duration_secs = found.duration_sec,
                match_score = found.score,
                reference_episode = episodes[first].0,
                "【片头片尾】该集片尾声纹匹配"
            );
            results.push(DetectedOutro {
                episode: *episode,
                outro_start_ms: *offset_ms + seconds_to_ms(found.start2_sec),
                outro_end_ms: *offset_ms + seconds_to_ms(found.end2_sec),
            });
        } else {
            tracing::debug!(
                episode,
                reference_episode = episodes[first].0,
                "【片头片尾】该集未匹配到参考片尾声纹"
            );
        }
    }
    tracing::info!(
        applied = results.len(),
        pairs_checked,
        reference_episode_a = episodes[first].0,
        reference_episode_b = episodes[second].0,
        reference_a_start_sec = common.start1_sec,
        reference_a_end_sec = common.end1_sec,
        reference_b_start_sec = common.start2_sec,
        reference_b_end_sec = common.end2_sec,
        matched_duration_secs = common.duration_sec,
        match_score = common.score,
        elapsed_ms = started.elapsed().as_millis(),
        "【片头片尾】片尾声纹比对完成"
    );
    results
}

fn seconds_to_ms(seconds: f32) -> i64 {
    (seconds * 1000.0) as i64
}
