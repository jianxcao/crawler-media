use super::{ConsensusRange, PairMatch};

// Keep differently edited cuts in separate clusters even when one contains the other.
const MIN_INTERVAL_LENGTH_RATIO: f32 = 0.9;
const MIN_INTERVAL_OVERLAP_RATIO: f32 = 0.8;

pub(super) struct ClusterSummary {
    pub(super) matches: Vec<usize>,
    pub(super) supporting_episodes: usize,
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
    let tolerance =
        crate::SamplingPolicy::default().max_reference_boundary_delta_ms as f32 / 1000.0;
    if (left.0 - right.0).abs() > tolerance || (left.1 - right.1).abs() > tolerance {
        return false;
    }
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
    episode_numbers: &[u32],
) -> Option<ClusterSummary> {
    if cluster.is_empty() {
        return None;
    }
    let mut seen_episodes = std::collections::HashSet::new();
    for index in cluster {
        let matched = &matches[*index];
        seen_episodes.insert(episode_numbers[matched.first]);
        seen_episodes.insert(episode_numbers[matched.second]);
    }
    let supporting_episodes = seen_episodes.len();
    Some(ClusterSummary {
        matches: cluster.to_vec(),
        supporting_episodes,
    })
}

pub(super) fn ranges_for_cluster(
    cluster: &[usize],
    matches: &[PairMatch],
    episode_numbers: &[u32],
) -> Vec<ConsensusRange> {
    let mut intervals = vec![Vec::new(); episode_numbers.len()];
    for index in cluster {
        let matched = &matches[*index];
        if let Some(interval) = interval_for(matched, matched.first) {
            intervals[matched.first].push((
                episode_numbers[matched.second],
                (interval.0, interval.1, matched.segment.score),
            ));
        }
        if let Some(interval) = interval_for(matched, matched.second) {
            intervals[matched.second].push((
                episode_numbers[matched.first],
                (interval.0, interval.1, matched.segment.score),
            ));
        }
    }
    intervals
        .into_iter()
        .enumerate()
        .flat_map(|(episode_index, values)| target_hypotheses(episode_index, &values))
        .collect()
}

fn target_hypotheses(
    episode_index: usize,
    alternatives: &[(u32, (f32, f32, f64))],
) -> Vec<ConsensusRange> {
    let mut starts = alternatives.iter().map(|(_, v)| v.0).collect::<Vec<_>>();
    let mut ends = alternatives.iter().map(|(_, v)| v.1).collect::<Vec<_>>();
    starts.sort_by(f32::total_cmp);
    ends.sort_by(f32::total_cmp);
    starts.dedup();
    ends.dedup();
    let tolerance =
        crate::SamplingPolicy::default().max_reference_boundary_delta_ms as f32 / 1000.0;
    let mut hypotheses = Vec::new();
    // A component can connect through reference coordinates while its target occurrences
    // disagree. Enumerate compatible target boundary windows before choosing one occurrence
    // per reference. Observed lower boundaries cover every compatible set without a power set.
    for start in starts {
        for &end in &ends {
            let mut values = std::collections::BTreeMap::new();
            for &(episode, interval) in alternatives {
                if interval.0 >= start
                    && interval.0 - start <= tolerance
                    && interval.1 >= end
                    && interval.1 - end <= tolerance
                {
                    record_reference(&mut values, episode, (interval.0, interval.1), interval.2);
                }
            }
            if let Some(range) = range_for_hypothesis(episode_index, values) {
                hypotheses.push(range);
            }
        }
    }
    hypotheses
}

fn range_for_hypothesis(
    episode_index: usize,
    values: std::collections::BTreeMap<u32, (f32, f32, f64)>,
) -> Option<ConsensusRange> {
    if values.len() < 2 {
        return None;
    }
    let start_secs = median_sorted(values.values().map(|v| v.0).collect())?;
    let end_secs = median_sorted(values.values().map(|v| v.1).collect())?;
    let boundary_spread_secs = values
        .values()
        .map(|v| (v.0 - start_secs).abs() + (v.1 - end_secs).abs())
        .fold(0.0, f32::max);
    Some(ConsensusRange {
        episode_index,
        start_secs,
        end_secs,
        supporting_pairs: values.len(),
        score: values.values().map(|v| v.2).fold(0.0, f64::max),
        boundary_spread_secs,
    })
}

fn record_reference(
    values: &mut std::collections::BTreeMap<u32, (f32, f32, f64)>,
    episode: u32,
    interval: (f32, f32),
    score: f64,
) {
    let candidate = (interval.0, interval.1, score);
    values
        .entry(episode)
        .and_modify(|current| {
            if candidate
                .2
                .total_cmp(&current.2)
                .then_with(|| (current.1 - current.0).total_cmp(&(candidate.1 - candidate.0)))
                .then(candidate.0.total_cmp(&current.0))
                .then(candidate.1.total_cmp(&current.1))
                .is_lt()
            {
                *current = candidate;
            }
        })
        .or_insert(candidate);
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
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}
