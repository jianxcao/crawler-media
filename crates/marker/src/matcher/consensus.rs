use std::cmp::Ordering;

use crate::types::CommonSegment;

mod cluster;
use cluster::{
    compare_cluster_summaries, match_clusters, median, ranges_for_cluster, summarize_cluster,
};

const MIN_SEASON_SUPPORT_PERCENT: usize = 20;
const MIN_SEASON_PAIR_SUPPORT_PERCENT: usize = 2;

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
}

pub(super) struct SeasonConsensus {
    pub ranges: Vec<ConsensusRange>,
    pub pairs_checked: usize,
    pub candidate_pairs: usize,
    pub rejected_episode_support_clusters: usize,
    pub rejected_pair_support_clusters: usize,
    pub supporting_episodes: usize,
    pub supporting_pairs: usize,
    pub minimum_support: usize,
    pub minimum_pair_support: usize,
    pub median_duration_secs: f32,
    pub consensus_clusters: usize,
}

pub(super) fn find_season_consensus(
    episode_count: usize,
    mut compare: impl FnMut(usize, usize) -> Option<CommonSegment>,
) -> SeasonConsensus {
    let mut matches = Vec::new();
    let mut pairs_checked = 0;
    for first in 0..episode_count {
        for second in first + 1..episode_count {
            pairs_checked += 1;
            if let Some(segment) = compare(first, second) {
                matches.push(PairMatch {
                    first,
                    second,
                    segment,
                });
            }
        }
    }

    let minimum_support = minimum_support(episode_count);
    let minimum_pair_support = minimum_pair_support(episode_count);
    let candidate_pairs = matches.len();
    let clusters = match_clusters(&matches);
    let mut rejected_episode_support_clusters = 0;
    let mut rejected_pair_support_clusters = 0;
    let mut summaries = Vec::new();
    for summary in clusters
        .iter()
        .filter_map(|cluster| summarize_cluster(cluster, &matches, episode_count))
    {
        if summary.supporting_episodes < minimum_support {
            rejected_episode_support_clusters += 1;
        } else if summary.matches.len() < minimum_pair_support {
            rejected_pair_support_clusters += 1;
        } else {
            summaries.push(summary);
        }
    }
    if summaries.is_empty() {
        return SeasonConsensus {
            ranges: Vec::new(),
            pairs_checked,
            candidate_pairs,
            rejected_episode_support_clusters,
            rejected_pair_support_clusters,
            supporting_episodes: 0,
            supporting_pairs: 0,
            minimum_support,
            minimum_pair_support,
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
                    candidate_duration.total_cmp(&current_duration) == Ordering::Greater
                        || (candidate_duration == current_duration
                            && compare_cluster_summaries(summary, &summaries[*current_index])
                                == Ordering::Greater)
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
        .map(|index| summaries[*index].matches.len())
        .sum();
    SeasonConsensus {
        supporting_episodes: ranges.len(),
        supporting_pairs,
        median_duration_secs,
        consensus_clusters: selected_clusters.len(),
        ranges,
        pairs_checked,
        candidate_pairs,
        rejected_episode_support_clusters,
        rejected_pair_support_clusters,
        minimum_support,
        minimum_pair_support,
    }
}

fn minimum_support(episode_count: usize) -> usize {
    if episode_count < 6 {
        return 2;
    }
    let percent = (episode_count * MIN_SEASON_SUPPORT_PERCENT).div_ceil(100);
    percent.max(3)
}

fn minimum_pair_support(episode_count: usize) -> usize {
    let possible_pairs = episode_count.saturating_mul(episode_count.saturating_sub(1)) / 2;
    if possible_pairs == 0 {
        return 0;
    }
    let percent = (possible_pairs * MIN_SEASON_PAIR_SUPPORT_PERCENT).div_ceil(100);
    percent.max(3).min(possible_pairs)
}
