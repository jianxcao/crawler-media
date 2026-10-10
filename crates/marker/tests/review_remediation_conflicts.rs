mod common;
use common::{make_target_evidence, MockVerificationEngine};
use marker::{CommonSegment, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel,
    TemplateReference, VerificationOutcome, verify_template_window};

#[test]
fn conflicting_templates_are_unresolved_in_every_model_order() {
    let target = make_target_evidence("target", 9, SegmentKind::Intro, 9000, 0, 180_000);
    let mut engine = MockVerificationEngine::default();
    let mut ctx = TemplateContext::default();
    for (index, (episodes, start)) in [([1, 2], 10.0), ([3, 4], 100.0)].into_iter().enumerate() {
        let mut refs = Vec::new();
        for episode in episodes {
            let id = format!("reference-{episode}"); let tag = episode * 1000;
            let ev = make_target_evidence(&id, episode, SegmentKind::Intro, tag, 0, 180_000);
            engine.returns.insert((9000, tag), vec![CommonSegment {
                start1_sec: start, end1_sec: start + 60., start2_sec: 10., end2_sec: 70.,
                duration_sec: 60., score: 1., }]);
            refs.push(TemplateReference { sample_id: id.clone(), ledger_id: ev.ledger_id.clone(),
                episode, source_version: ev.source_version.clone(), match_interval_ms: (10_000, 70_000),
                match_from_end_ms: None });
            ctx.references.insert(id, ev);
        }
        ctx.models.push(TemplateModel { model_id: format!("model-{index}"), version: 1,
            kind: SegmentKind::Intro, references: refs, expected_duration_ms: 60_000, is_stable: true });
    }
    let first = verify_template_window(&engine, &target, &ctx, &SamplingPolicy::default());
    ctx.models.reverse();
    let second = verify_template_window(&engine, &target, &ctx, &SamplingPolicy::default());
    assert!(matches!(first, VerificationOutcome::NeedsFullWindow { .. }) &&
        matches!(second, VerificationOutcome::NeedsFullWindow { .. }),
        "incompatible equally supported templates must not pick the first: {first:?}, {second:?}");
}
