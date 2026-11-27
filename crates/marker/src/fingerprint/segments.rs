use rusty_chromaprint::{Configuration, Segment, match_fingerprints};

use crate::types::CommonSegment;

const MAX_ADJACENT_SCORE_DELTA: f64 = 4.0;
const MAX_BRIDGE_WEAK_ITEMS: usize = 16;
const MAX_RAW_BRIDGE_GAP_ITEMS: usize = 24;
const MIN_BRIDGE_FLANK_ITEMS: usize = 40;
const STUDIO_BUMPER_MAX_SECS: f32 = 6.0;
const FULL_THEME_MIN_QUERY_SECS: f32 = 10.0;

pub(super) fn find_common_segment(
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Option<CommonSegment> {
    find_common_segments(first, second, min_duration_secs, max_duration_secs)
        .into_iter()
        .next()
}

pub(super) fn find_common_segments(
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Vec<CommonSegment> {
    match compare_fingerprints(first, second, min_duration_secs, max_duration_secs) {
        super::PairFingerprintDecision::Candidates(candidates) => candidates,
        _ => Vec::new(),
    }
}

pub(super) fn compare_fingerprints(
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> super::PairFingerprintDecision {
    use super::PairFingerprintDecision;
    if crate::adaptive::model::is_constant_or_silent(first)
        || crate::adaptive::model::is_constant_or_silent(second)
    {
        return PairFingerprintDecision::Uncertain("constant_or_silent_fingerprint".into());
    }
    let config = Configuration::preset_test2();
    let segments = match match_fingerprints(first, second, &config) {
        Ok(segments) => segments,
        Err(error) => {
            return PairFingerprintDecision::Uncertain(format!(
                "native_comparison_failed: {error}"
            ));
        }
    };
    if segments.is_empty() {
        return PairFingerprintDecision::NoMatch;
    }
    let raw_max_secs = segments
        .iter()
        .map(|segment| segment.duration(&config))
        .fold(0.0f32, f32::max);
    let policy = crate::SamplingPolicy::default();
    let segments = segments
        .into_iter()
        .flat_map(|segment| split_weak_runs(segment, first, second, &config, &policy))
        .filter(|s| {
            let acceptable = s.score.is_finite() && s.score <= policy.max_score;
            if !acceptable {
                tracing::debug!(score = s.score, "合并前拒绝低质量声纹片段");
            }
            acceptable
        })
        .collect();
    let merged = merge_adjacent_segments(
        segments,
        first,
        second,
        &config,
        policy.max_score,
        max_duration_secs,
    );
    let merged_max_secs = merged
        .iter()
        .map(|segment| segment.duration(&config))
        .fold(raw_max_secs, f32::max);
    let mut candidates = merged
        .into_iter()
        .map(|segment| CommonSegment {
            start1_sec: segment.start1(&config),
            end1_sec: segment.end1(&config),
            start2_sec: segment.start2(&config),
            end2_sec: segment.end2(&config),
            duration_sec: segment.duration(&config),
            score: segment.score,
        })
        .filter(|candidate| {
            crate::candidate_quality::acceptable_segment(
                candidate,
                min_duration_secs,
                max_duration_secs,
                policy.max_score,
            )
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        left.score
            .total_cmp(&right.score)
            .then_with(|| right.duration_sec.total_cmp(&left.duration_sec))
    });
    let candidates = deduplicate_candidates(candidates);
    if candidates.is_empty() {
        if min_duration_secs >= FULL_THEME_MIN_QUERY_SECS
            && merged_max_secs < STUDIO_BUMPER_MAX_SECS
        {
            PairFingerprintDecision::NoMatch
        } else {
            PairFingerprintDecision::Uncertain("native_candidates_rejected".into())
        }
    } else {
        PairFingerprintDecision::Candidates(candidates)
    }
}

// The upstream matcher can return an averaged segment without a gradient peak at a weak
// transition. Inspect sustained weak runs before merging so a long good region cannot hide it.
fn split_weak_runs(
    segment: Segment,
    first: &[u32],
    second: &[u32],
    config: &Configuration,
    policy: &crate::SamplingPolicy,
) -> Vec<Segment> {
    let Some(end1) = segment.offset1.checked_add(segment.items_count) else {
        return Vec::new();
    };
    let Some(end2) = segment.offset2.checked_add(segment.items_count) else {
        return Vec::new();
    };
    let (Some(first), Some(second)) = (
        first.get(segment.offset1..end1),
        second.get(segment.offset2..end2),
    ) else {
        tracing::warn!("声纹候选偏移超出输入边界");
        return Vec::new();
    };
    let scores = first
        .iter()
        .zip(second)
        .map(|(a, b)| (a ^ b).count_ones() as f64)
        .collect::<Vec<_>>();
    let min_weak_items = ((policy.max_internal_gap_ms as f32 / 1000.0)
        / config.item_duration_in_seconds())
    .ceil()
    .max(1.0) as usize;
    let mut pieces = Vec::new();
    let mut begin = 0;
    let mut index = 0;
    while index < scores.len() {
        if scores[index] <= policy.max_score {
            index += 1;
            continue;
        }
        let weak_start = index;
        while index < scores.len() && scores[index] > policy.max_score {
            index += 1;
        }
        if index - weak_start >= min_weak_items {
            if begin < weak_start {
                pieces.push(segment_piece(&segment, &scores, begin, weak_start));
            }
            tracing::debug!(
                items = index - weak_start,
                "候选含持续低质量声纹，禁止跨段合并"
            );
            begin = index;
        }
    }
    if begin < scores.len() {
        pieces.push(segment_piece(&segment, &scores, begin, scores.len()));
    }
    pieces
}

fn segment_piece(segment: &Segment, scores: &[f64], start: usize, end: usize) -> Segment {
    Segment {
        offset1: segment.offset1 + start,
        offset2: segment.offset2 + start,
        items_count: end - start,
        score: scores[start..end].iter().sum::<f64>() / (end - start) as f64,
    }
}

fn deduplicate_candidates(candidates: Vec<CommonSegment>) -> Vec<CommonSegment> {
    let mut unique: Vec<CommonSegment> = Vec::with_capacity(candidates.len());
    'candidate: for candidate in candidates {
        for existing in &unique {
            if same_occurrence(
                (candidate.start1_sec, candidate.end1_sec),
                (existing.start1_sec, existing.end1_sec),
            ) && same_occurrence(
                (candidate.start2_sec, candidate.end2_sec),
                (existing.start2_sec, existing.end2_sec),
            ) {
                continue 'candidate;
            }
        }
        unique.push(candidate);
    }
    unique
}

fn same_occurrence(left: (f32, f32), right: (f32, f32)) -> bool {
    let shorter = (left.1 - left.0).min(right.1 - right.0);
    let longer = (left.1 - left.0).max(right.1 - right.0);
    if shorter <= 0.0 || longer <= 0.0 || shorter / longer < 0.9 {
        return false;
    }
    let overlap = (left.1.min(right.1) - left.0.max(right.0)).max(0.0);
    overlap / shorter >= 0.9
}

fn has_sustained_weak_run(slice1: &[u32], slice2: &[u32], max_score: f64, limit: usize) -> bool {
    let mut consecutive = 0usize;
    for (a, b) in slice1.iter().zip(slice2) {
        if (a ^ b).count_ones() as f64 > max_score {
            consecutive += 1;
            if consecutive >= limit {
                return true;
            }
        } else {
            consecutive = 0;
        }
    }
    false
}

// Chromaprint's gradient peak detector erodes ~3 matching items on each side of a spike,
// so measure the true non-matching gap span (capped at <= 16 items, ~2.0s).
fn effective_weak_gap_items(gap1: &[u32], gap2: &[u32], max_score: f64) -> usize {
    let mut first_weak = None;
    let mut last_weak = 0usize;
    for (idx, (a, b)) in gap1.iter().zip(gap2).enumerate() {
        if (a ^ b).count_ones() as f64 > max_score {
            if first_weak.is_none() {
                first_weak = Some(idx);
            }
            last_weak = idx;
        }
    }
    first_weak.map_or(0, |start| last_weak - start + 1)
}

fn merge_adjacent_segments(
    mut segments: Vec<Segment>,
    first: &[u32],
    second: &[u32],
    config: &Configuration,
    max_score: f64,
    max_duration_secs: f32,
) -> Vec<Segment> {
    segments.sort_by_key(|segment| {
        (
            segment.offset2 as isize - segment.offset1 as isize,
            segment.offset1,
            segment.offset2,
        )
    });
    let mut merged: Vec<Segment> = Vec::with_capacity(segments.len());

    for segment in segments {
        let Some(previous) = merged.last_mut() else {
            merged.push(segment);
            continue;
        };
        let end1 = previous.offset1 + previous.items_count;
        let end2 = previous.offset2 + previous.items_count;
        let Some(gap1) = segment.offset1.checked_sub(end1) else {
            merged.push(segment);
            continue;
        };
        let Some(gap2) = segment.offset2.checked_sub(end2) else {
            merged.push(segment);
            continue;
        };
        if (previous.score - segment.score).abs() >= MAX_ADJACENT_SCORE_DELTA {
            merged.push(segment);
            continue;
        }
        if gap1 == 0 && gap2 == 0 {
            let new_count = segment.offset1 + segment.items_count - previous.offset1;
            // Preserve the weakest constituent quality instead of diluting it by duration.
            previous.score = previous.score.max(segment.score);
            previous.items_count = new_count;
            continue;
        }
        let can_bridge = gap1 == gap2
            && gap1 <= MAX_RAW_BRIDGE_GAP_ITEMS
            && previous.items_count >= MIN_BRIDGE_FLANK_ITEMS
            && segment.items_count >= MIN_BRIDGE_FLANK_ITEMS
            && previous.score <= 2.5
            && segment.score <= 2.5
            && (previous.score - segment.score).abs() <= 1.5;
        if !can_bridge {
            merged.push(segment);
            continue;
        }
        let new_count = segment.offset1 + segment.items_count - previous.offset1;
        let candidate_merged = Segment {
            offset1: previous.offset1,
            offset2: previous.offset2,
            items_count: new_count,
            score: previous.score,
        };
        if candidate_merged.duration(config) > max_duration_secs {
            merged.push(segment);
            continue;
        }
        let (
            Some(left_flank),
            Some(right_flank),
            Some(gap_slice1),
            Some(gap_slice2),
            Some(slice1),
            Some(slice2),
        ) = (
            first.get(previous.offset1..end1),
            first.get(segment.offset1..segment.offset1 + segment.items_count),
            first.get(end1..segment.offset1),
            second.get(end2..segment.offset2),
            first.get(previous.offset1..previous.offset1 + new_count),
            second.get(previous.offset2..previous.offset2 + new_count),
        )
        else {
            merged.push(segment);
            continue;
        };
        if effective_weak_gap_items(gap_slice1, gap_slice2, max_score) > MAX_BRIDGE_WEAK_ITEMS
            || crate::adaptive::model::is_constant_or_silent(left_flank)
            || crate::adaptive::model::is_constant_or_silent(right_flank)
            || has_sustained_weak_run(slice1, slice2, max_score, MAX_BRIDGE_WEAK_ITEMS)
        {
            merged.push(segment);
            continue;
        }
        let bridged_score = slice1
            .iter()
            .zip(slice2)
            .map(|(a, b)| (a ^ b).count_ones() as f64)
            .sum::<f64>()
            / new_count as f64;
        if !bridged_score.is_finite() || bridged_score > max_score {
            merged.push(segment);
            continue;
        }
        previous.score = bridged_score.max(previous.score).max(segment.score);
        previous.items_count = new_count;
    }
    merged
}
