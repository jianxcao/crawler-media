use std::cmp::Ordering;

use crate::types::CommonSegment;

mod cluster;
use cluster::{
    compare_cluster_summaries, match_clusters, median, ranges_for_cluster, summarize_cluster,
};

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
}

pub(super) struct SeasonConsensus {
    pub ranges: Vec<ConsensusRange>,
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

pub(super) fn find_season_consensus<F, C>(episode_count: usize, mut compare: F) -> SeasonConsensus
where
    F: FnMut(usize, usize) -> C,
    C: IntoIterator<Item = CommonSegment>,
{
    let mut matches = Vec::new();
    let mut pairs_checked = 0;
    let mut candidate_pairs = 0;
    for first in 0..episode_count {
        for second in first + 1..episode_count {
            pairs_checked += 1;
            let pair_started = std::time::Instant::now();
            let candidates = compare(first, second).into_iter().collect::<Vec<_>>();
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
                matches.push(PairMatch {
                    first,
                    second,
                    segment,
                });
            }
        }
    }

    let minimum_support = minimum_support(episode_count);
    let candidate_segments = matches.len();
    let clusters = match_clusters(&matches);
    let mut rejected_episode_support_clusters = 0;
    let mut summaries = Vec::new();
    for summary in clusters
        .iter()
        .filter_map(|cluster| summarize_cluster(cluster, &matches, episode_count))
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

    let mut selected_ranges: Vec<Option<(usize, ConsensusRange)>> =
        (0..episode_count).map(|_| None).collect();
    for (cluster_index, summary) in summaries.iter().enumerate() {
        for range in ranges_for_cluster(&summary.matches, &matches, episode_count) {
            let episode_index = range.episode_index;
            let replace = selected_ranges[episode_index].as_ref().is_none_or(
                |(current_index, current_range)| {
                    let candidate_duration = range.end_secs - range.start_secs;
                    let current_duration = current_range.end_secs - current_range.start_secs;
                    range.supporting_pairs > current_range.supporting_pairs
                        || (range.supporting_pairs == current_range.supporting_pairs
                            && (compare_cluster_summaries(summary, &summaries[*current_index])
                                == Ordering::Greater
                                || (compare_cluster_summaries(
                                    summary,
                                    &summaries[*current_index],
                                ) == Ordering::Equal
                                    && candidate_duration.total_cmp(&current_duration)
                                        == Ordering::Greater)))
                },
            );
            if replace {
                selected_ranges[episode_index] = Some((cluster_index, range));
            }
        }
    }
    let selected = selected_ranges.into_iter().flatten().collect::<Vec<_>>();
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
        supporting_episodes: ranges.len(),
        supporting_pairs,
        median_duration_secs,
        consensus_clusters: selected_clusters.len(),
        ranges,
        pairs_checked,
        candidate_pairs,
        candidate_segments,
        rejected_episode_support_clusters,
        minimum_support,
    }
}

fn minimum_support(episode_count: usize) -> usize {
    if episode_count < 6 {
        return 2;
    }
    let percent = (episode_count * MIN_SEASON_SUPPORT_PERCENT).div_ceil(100);
    percent.max(3)
}
