use std::cmp::Ordering;

use super::{ConsensusRange, PairMatch};

// Keep differently edited cuts in separate clusters even when one contains the other.
const MIN_INTERVAL_LENGTH_RATIO: f32 = 0.9;
const MIN_INTERVAL_OVERLAP_RATIO: f32 = 0.8;

pub(super) struct ClusterSummary {
    pub(super) matches: Vec<usize>,
    pub(super) supporting_episodes: usize,
    median_duration_secs: f32,
    average_score: f64,
}

pub(super) fn match_clusters(matches: &[PairMatch]) -> Vec<Vec<usize>> {
    let mut visited = vec![false; matches.len()];
    let mut clusters = Vec::new();
    for start in 0..matches.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut pending = vec![start];
        let mut cluster = Vec::new();
        while let Some(current) = pending.pop() {
            cluster.push(current);
            for candidate in 0..matches.len() {
                if visited[candidate]
                    || !matches_same_episode_interval(&matches[current], &matches[candidate])
                {
                    continue;
                }
                visited[candidate] = true;
                pending.push(candidate);
            }
        }
        clusters.push(cluster);
    }
    clusters
}

fn matches_same_episode_interval(first: &PairMatch, second: &PairMatch) -> bool {
    for episode in [first.first, first.second] {
        let Some(left) = interval_for(first, episode) else {
            continue;
        };
        let Some(right) = interval_for(second, episode) else {
            continue;
        };
        if intervals_overlap(left, right) {
            return true;
        }
    }
    false
}

fn interval_for(matched: &PairMatch, episode: usize) -> Option<(f32, f32)> {
    if episode == matched.first {
        Some((matched.segment.start1_sec, matched.segment.end1_sec))
    } else if episode == matched.second {
        Some((matched.segment.start2_sec, matched.segment.end2_sec))
    } else {
        None
    }
}

fn intervals_overlap(left: (f32, f32), right: (f32, f32)) -> bool {
    let left_duration = left.1 - left.0;
    let right_duration = right.1 - right.0;
    let shorter = left_duration.min(right_duration);
    let longer = left_duration.max(right_duration);
    if shorter <= 0.0 || longer <= 0.0 || shorter / longer < MIN_INTERVAL_LENGTH_RATIO {
        return false;
    }
    let overlap = (left.1.min(right.1) - left.0.max(right.0)).max(0.0);
    overlap / shorter >= MIN_INTERVAL_OVERLAP_RATIO
}

pub(super) fn summarize_cluster(
    cluster: &[usize],
    matches: &[PairMatch],
    episode_count: usize,
) -> Option<ClusterSummary> {
    if cluster.is_empty() {
        return None;
    }
    let mut seen_episodes = vec![false; episode_count];
    let mut durations = Vec::with_capacity(cluster.len());
    let mut total_score = 0.0;
    for index in cluster {
        let matched = &matches[*index];
        seen_episodes[matched.first] = true;
        seen_episodes[matched.second] = true;
        durations.push(matched.segment.duration_sec);
        total_score += matched.segment.score;
    }
    durations.sort_by(f32::total_cmp);
    let median_duration_secs = median(&durations)?;
    let supporting_episodes = seen_episodes.into_iter().filter(|seen| *seen).count();
    Some(ClusterSummary {
        matches: cluster.to_vec(),
        supporting_episodes,
        median_duration_secs,
        average_score: total_score / cluster.len() as f64,
    })
}

pub(super) fn compare_cluster_summaries(left: &ClusterSummary, right: &ClusterSummary) -> Ordering {
    let left_coverage = left.supporting_episodes as f32 * left.median_duration_secs;
    let right_coverage = right.supporting_episodes as f32 * right.median_duration_secs;
    left_coverage
        .total_cmp(&right_coverage)
        .then_with(|| left.supporting_episodes.cmp(&right.supporting_episodes))
        .then_with(|| left.matches.len().cmp(&right.matches.len()))
        .then_with(|| right.average_score.total_cmp(&left.average_score))
}

pub(super) fn ranges_for_cluster(
    cluster: &[usize],
    matches: &[PairMatch],
    episode_count: usize,
) -> Vec<ConsensusRange> {
    let mut intervals = vec![Vec::new(); episode_count];
    let mut supporting_pairs = vec![std::collections::HashSet::new(); episode_count];
    for index in cluster {
        let matched = &matches[*index];
        if let Some(interval) = interval_for(matched, matched.first) {
            intervals[matched.first].push(interval);
            supporting_pairs[matched.first].insert(matched.second);
        }
        if let Some(interval) = interval_for(matched, matched.second) {
            intervals[matched.second].push(interval);
            supporting_pairs[matched.second].insert(matched.first);
        }
    }
    intervals
        .into_iter()
        .enumerate()
        .filter_map(|(episode_index, values)| {
            let starts = values.iter().map(|interval| interval.0).collect::<Vec<_>>();
            let ends = values.iter().map(|interval| interval.1).collect::<Vec<_>>();
            Some(ConsensusRange {
                episode_index,
                start_secs: median_sorted(starts)?,
                end_secs: median_sorted(ends)?,
                supporting_pairs: supporting_pairs[episode_index].len(),
            })
        })
        .collect()
}

pub(super) fn median(values: &[f32]) -> Option<f32> {
    median_sorted(values.to_vec())
}

fn median_sorted(mut values: Vec<f32>) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}
