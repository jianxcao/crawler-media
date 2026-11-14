use std::cmp::Ordering;

use crate::PairFingerprintDecision;
use crate::types::CommonSegment;

mod cluster;
use cluster::{match_clusters, median, ranges_for_cluster, summarize_cluster};

const MIN_SEASON_SUPPORT_PERCENT: usize = 20;

#[derive(Clone)]
struct PairMatch {
    first: usize,
    second: usize,
    segment: CommonSegment,
}

pub(super) struct ConsensusRange {
    pub episode_index: usize,
    pub start_secs: f32,
    pub end_secs: f32,
    pub supporting_pairs: usize,
    score: f64,
    boundary_spread_secs: f32,
}

pub(super) struct SeasonConsensus {
    pub ranges: Vec<ConsensusRange>,
    pub raw_candidates: Vec<bool>,
    pub uncertain_reasons: Vec<std::collections::HashSet<String>>,
    pub empty_references: Vec<std::collections::HashSet<usize>>,
    pub pairs_checked: usize,
    pub candidate_pairs: usize,
    pub candidate_segments: usize,
    pub rejected_episode_support_clusters: usize,
    pub supporting_episodes: usize,
    pub supporting_pairs: usize,
    pub minimum_support: usize,
    pub median_duration_secs: f32,
    pub consensus_clusters: usize,
}

struct PairComparisons {
    matches: Vec<PairMatch>,
    pairs_checked: usize,
    candidate_pairs: usize,
    raw_candidates: Vec<bool>,
    empty_references: Vec<std::collections::HashSet<usize>>,
    uncertain_reasons: Vec<std::collections::HashSet<String>>,
}

pub(super) fn find_season_consensus<F>(
    episode_numbers: &[u32],
    min_duration: f32,
    max_duration: f32,
    compare: F,
) -> SeasonConsensus
where
    F: FnMut(usize, usize) -> PairFingerprintDecision,
{
    let episode_count = episode_numbers.len();
    let PairComparisons {
        matches,
        pairs_checked,
        candidate_pairs,
        raw_candidates,
        empty_references,
        uncertain_reasons,
    } = collect_pair_matches(episode_numbers, min_duration, max_duration, compare);

    let minimum_support = minimum_support(
        episode_numbers
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len(),
    );
    let candidate_segments = matches.len();
    let clusters = match_clusters(&matches);
    let mut rejected_episode_support_clusters = 0;
    let mut summaries = Vec::new();
    for summary in clusters
        .iter()
        .filter_map(|cluster| summarize_cluster(cluster, &matches, episode_numbers))
    {
        if summary.supporting_episodes < minimum_support {
            rejected_episode_support_clusters += 1;
        } else {
            summaries.push(summary);
        }
    }
    if summaries.is_empty() {
        return SeasonConsensus {
            ranges: Vec::new(),
            raw_candidates,
            empty_references,
            uncertain_reasons,
            pairs_checked,
            candidate_pairs,
            candidate_segments,
            rejected_episode_support_clusters,
            supporting_episodes: 0,
            supporting_pairs: 0,
            minimum_support,
            median_duration_secs: 0.0,
            consensus_clusters: 0,
        };
    }

    let mut candidates: Vec<Vec<(usize, ConsensusRange)>> =
        (0..episode_count).map(|_| Vec::new()).collect();
    for (cluster_index, summary) in summaries.iter().enumerate() {
        for range in ranges_for_cluster(&summary.matches, &matches, episode_numbers) {
            candidates[range.episode_index].push((cluster_index, range));
        }
    }
    let selected = candidates
        .into_iter()
        .filter_map(select_unambiguous_range)
        .collect::<Vec<_>>();
    let selected_clusters = selected
        .iter()
        .map(|(cluster_index, _)| *cluster_index)
        .collect::<std::collections::HashSet<_>>();
    let ranges = selected
        .into_iter()
        .map(|(_, range)| range)
        .collect::<Vec<_>>();
    let mut selected_durations = ranges
        .iter()
        .map(|range| range.end_secs - range.start_secs)
        .collect::<Vec<_>>();
    selected_durations.sort_by(f32::total_cmp);
    let median_duration_secs = median(&selected_durations).unwrap_or_default();
    let supporting_pairs = selected_clusters
        .iter()
        .flat_map(|index| summaries[*index].matches.iter())
        .map(|index| (matches[*index].first, matches[*index].second))
        .collect::<std::collections::HashSet<_>>()
        .len();
    SeasonConsensus {
        supporting_episodes: ranges
            .iter()
            .map(|range| episode_numbers[range.episode_index])
            .collect::<std::collections::HashSet<_>>()
            .len(),
        supporting_pairs,
        median_duration_secs,
        consensus_clusters: selected_clusters.len(),
        ranges,
        raw_candidates,
        empty_references,
        uncertain_reasons,
        pairs_checked,
        candidate_pairs,
        candidate_segments,
        rejected_episode_support_clusters,
        minimum_support,
    }
}

fn collect_pair_matches<F>(
    episode_numbers: &[u32],
    min_duration: f32,
    max_duration: f32,
    mut compare: F,
) -> PairComparisons
where
    F: FnMut(usize, usize) -> PairFingerprintDecision,
{
    let mut matches = Vec::new();
    let mut raw_candidates = vec![false; episode_numbers.len()];
    let mut uncertain_reasons = vec![std::collections::HashSet::new(); episode_numbers.len()];
    let mut empty_references = vec![std::collections::HashSet::new(); episode_numbers.len()];
    let mut pairs_checked = 0;
    let mut candidate_pairs = 0;
    let policy = crate::SamplingPolicy::default();
    for first in 0..episode_numbers.len() {
        for second in first + 1..episode_numbers.len() {
            if episode_numbers[first] == episode_numbers[second] {
                continue;
            }
            pairs_checked += 1;
            let pair_started = std::time::Instant::now();
            let candidates = match compare(first, second) {
                PairFingerprintDecision::NoMatch => {
                    empty_references[first].insert(second);
                    empty_references[second].insert(first);
                    Vec::new()
                }
                PairFingerprintDecision::Candidates(candidates) => {
                    raw_candidates[first] = true;
                    raw_candidates[second] = true;
                    candidates
                }
                PairFingerprintDecision::Uncertain(reason) => {
                    raw_candidates[first] = true;
                    raw_candidates[second] = true;
                    uncertain_reasons[first].insert(reason.clone());
                    uncertain_reasons[second].insert(reason.clone());
                    tracing::debug!(first, second, reason, "声纹单集比较未决");
                    Vec::new()
                }
            };
            candidate_pairs += usize::from(!candidates.is_empty());
            tracing::debug!(
                first_episode_index = first,
                second_episode_index = second,
                candidates = candidates.len(),
                intervals = ?candidates.iter().map(|segment| (
                    segment.start1_sec,
                    segment.end1_sec,
                    segment.start2_sec,
                    segment.end2_sec,
                    segment.duration_sec,
                    segment.score,
                )).collect::<Vec<_>>(),
                elapsed_ms = pair_started.elapsed().as_millis() as u64,
                "【片头片尾】单集对比候选明细"
            );
            for segment in candidates {
                if !crate::candidate_quality::acceptable_segment(
                    &segment,
                    min_duration,
                    max_duration,
                    policy.max_score,
                ) {
                    tracing::debug!(
                        first,
                        second,
                        score = segment.score,
                        "拒绝低质量或无效区间候选"
                    );
                    continue;
                }
                matches.push(PairMatch {
                    first,
                    second,
                    segment,
                });
            }
        }
    }

    PairComparisons {
        matches,
        pairs_checked,
        candidate_pairs,
        raw_candidates,
        empty_references,
        uncertain_reasons,
    }
}

fn compare_range_evidence(left: &ConsensusRange, right: &ConsensusRange) -> Ordering {
    left.supporting_pairs
        .cmp(&right.supporting_pairs)
        .then_with(|| right.score.total_cmp(&left.score))
        .then_with(|| {
            right
                .boundary_spread_secs
                .total_cmp(&left.boundary_spread_secs)
        })
}

fn select_unambiguous_range(
    mut candidates: Vec<(usize, ConsensusRange)>,
) -> Option<(usize, ConsensusRange)> {
    candidates.sort_by(|a, b| {
        compare_range_evidence(&b.1, &a.1).then_with(|| {
            (b.1.end_secs - b.1.start_secs).total_cmp(&(a.1.end_secs - a.1.start_secs))
        })
    });
    let best = &candidates.first()?.1;
    let tolerance =
        crate::SamplingPolicy::default().max_reference_boundary_delta_ms as f32 / 1000.0;
    if candidates.iter().skip(1).any(|(_, other)| {
        compare_range_evidence(best, other) == Ordering::Equal
            && ((best.start_secs - other.start_secs).abs() > tolerance
                || (best.end_secs - other.end_secs).abs() > tolerance)
    }) {
        tracing::debug!(
            episode_index = best.episode_index,
            "同等证据区间冲突，保持未决"
        );
        return None;
    }
    candidates.into_iter().next()
}

fn minimum_support(episode_count: usize) -> usize {
    if episode_count < 6 {
        return 2;
    }
    let percent = (episode_count * MIN_SEASON_SUPPORT_PERCENT).div_ceil(100);
    percent.max(3)
}
