use rusty_chromaprint::{Configuration, Segment, match_fingerprints};

use crate::types::CommonSegment;

const MAX_ADJACENT_SCORE_DELTA: f64 = 4.0;

pub(super) fn find_common_segment(
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Option<CommonSegment> {
    let config = Configuration::preset_test2();
    let segments = match_fingerprints(first, second, &config).ok()?;
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
    candidates.into_iter().next()
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
