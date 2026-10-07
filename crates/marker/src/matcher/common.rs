use crate::types::CommonSegment;

pub(super) fn best_pair(
    count: usize,
    mut compare: impl FnMut(usize, usize) -> Option<CommonSegment>,
) -> (usize, Option<(usize, usize, CommonSegment)>) {
    let mut pairs_checked = 0;
    let mut best: Option<(usize, usize, CommonSegment)> = None;
    for first in 0..count {
        for second in (first + 1)..count {
            pairs_checked += 1;
            let Some(candidate) = compare(first, second) else {
                continue;
            };
            if best.as_ref().is_none_or(|(_, _, current)| {
                candidate.duration_sec > current.duration_sec
                    || (candidate.duration_sec == current.duration_sec
                        && candidate.score < current.score)
            }) {
                best = Some((first, second, candidate));
            }
        }
    }
    (pairs_checked, best)
}
