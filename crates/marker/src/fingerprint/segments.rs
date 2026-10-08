use rusty_chromaprint::{Configuration, Segment, match_fingerprints};

use crate::types::CommonSegment;

const MAX_ADJACENT_SCORE_DELTA: f64 = 4.0;

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
    let config = Configuration::preset_test2();
    let Ok(segments) = match_fingerprints(first, second, &config) else {
        return Vec::new();
    };
    let merged = merge_adjacent_segments(segments);
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
            candidate.duration_sec >= min_duration_secs
                && candidate.duration_sec <= max_duration_secs
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .duration_sec
            .partial_cmp(&left.duration_sec)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.score
                    .partial_cmp(&right.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    deduplicate_candidates(candidates)
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

fn merge_adjacent_segments(mut segments: Vec<Segment>) -> Vec<Segment> {
    segments.sort_by_key(|segment| (segment.offset1, segment.offset2));
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
        if gap1 > 1
            || gap1 != gap2
            || (previous.score - segment.score).abs() >= MAX_ADJACENT_SCORE_DELTA
        {
            merged.push(segment);
            continue;
        }

        let previous_count = previous.items_count;
        let new_count = segment.offset1 + segment.items_count - previous.offset1;
        let missing_score = 10.0 * gap1 as f64;
        previous.score = (previous.score * previous_count as f64
            + missing_score
            + segment.score * segment.items_count as f64)
            / new_count as f64;
        previous.items_count = new_count;
    }
    merged
}
