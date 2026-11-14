use crate::CommonSegment;

/// Quality shared by raw engine adapters and both automatic publication paths.
pub(crate) fn acceptable_segment(
    segment: &CommonSegment,
    min_duration_secs: f32,
    max_duration_secs: f32,
    max_score: f64,
) -> bool {
    let coordinates = [
        segment.start1_sec,
        segment.end1_sec,
        segment.start2_sec,
        segment.end2_sec,
    ];
    segment.score.is_finite()
        && segment.score <= max_score
        && coordinates.iter().all(|v| v.is_finite() && *v >= 0.0)
        && segment.end1_sec > segment.start1_sec
        && segment.end2_sec > segment.start2_sec
        && segment.duration_sec.is_finite()
        && segment.duration_sec >= min_duration_secs
        && segment.duration_sec <= max_duration_secs
        && ((segment.end1_sec - segment.start1_sec) - segment.duration_sec).abs() <= 0.01
        && ((segment.end2_sec - segment.start2_sec) - segment.duration_sec).abs() <= 0.01
}
