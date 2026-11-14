use marker::boundary::{
    BoundaryDecision, BoundaryEvidence, BoundaryPolicy, Interval, refine_boundary,
};

#[test]
fn chapter_corroboration_cannot_replace_required_reference_context() {
    let evidence = BoundaryEvidence {
        candidate: Interval {
            start_ms: 30_000,
            end_ms: 90_000,
        },
        complete_pcm: true,
        complete_context: true,
        chapter: Some(Interval {
            start_ms: 30_500,
            end_ms: 89_900,
        }),
        audio_transition: Some(Interval {
            start_ms: 30_800,
            end_ms: 90_000,
        }),
        references_agree: true,
        complete_guards: false,
        conflicting_evidence: false,
    };
    assert!(
        matches!(
            refine_boundary(&evidence, &BoundaryPolicy::default()),
            BoundaryDecision::Uncertain(_)
        ),
        "missing required reference PCM context cannot be supplied by chapter agreement"
    );
}
