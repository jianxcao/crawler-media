use marker::{
    EpisodeEvidence, FingerprintEngine, SamplingPolicy, SegmentKind, TemplateContext,
    VerificationOutcome, VerifiedInterval, verify_template_window,
};

#[test]
fn center_only_match_requires_full_window() {
    // 20s match in middle of 90s template requires full window
}

#[test]
fn intro_zero_start_is_valid_with_other_boundary_evidence() {
    // When intro legitimately starts at 0ms, it is valid as long as coverage and other boundary requirements match
}

#[test]
fn window_edge_match_is_not_a_complete_template() {
    // If matching interval hits the edge of sample without sufficient guard margin and without hitting media edge,
    // it requires full window.
}

#[test]
fn edited_middle_and_conflicting_references_require_fallback() {
    // If two references give conflicting boundaries delta > 1000ms, fallback to full window
}
