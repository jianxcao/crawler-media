mod common;

use common::{
    MockVerificationEngine, common_segment, intro_template_model, make_target_evidence,
    template_reference,
};
use marker::boundary::evidence::{complete_guards, complete_pcm};
use marker::boundary::{BoundaryPolicy, Interval};
use marker::matcher::{FingerprintDecision, verify_season_fingerprints_with};
use marker::{
    EpisodeEvidence, SamplingPolicy, SegmentKind, TemplateContext, VerificationOutcome,
    verify_template_window,
};

fn set_proven_origin(e: &mut EpisodeEvidence, origin: Option<i64>) {
    // Exercise the persisted evidence seam; unknown metadata is never inferred
    // from the sample request or the total PCM length.
    let mut value = serde_json::to_value(&e.capture.metrics).unwrap();
    value["contiguous_pcm_start_ms"] = serde_json::json!(origin);
    e.capture.metrics = serde_json::from_value(value).unwrap();
}

fn inputs(pcm: i64) -> Vec<EpisodeEvidence> {
    (1..=3)
        .map(|episode| {
            let mut e = make_target_evidence(
                &format!("sample-{episode}"),
                episode,
                SegmentKind::Intro,
                episode * 1000,
                0,
                180_000,
            );
            e.capture.pcm_duration_ms = Some(pcm);
            set_proven_origin(&mut e, Some(0));
            e
        })
        .collect()
}

fn engine(start: f32, end: f32) -> MockVerificationEngine {
    let mut engine = MockVerificationEngine::default();
    for (a, b) in [(1000, 2000), (1000, 3000), (2000, 3000)] {
        engine.returns.insert(
            (a, b),
            vec![common_segment((start, end), (start, end), 1.0)],
        );
    }
    engine
}

fn templates(e: &[EpisodeEvidence], start: i64, end: i64) -> TemplateContext {
    let mut context = TemplateContext::default();
    context.models.push(intro_template_model(
        "intro",
        e[1..]
            .iter()
            .map(|r| template_reference(&r.sample_id, r.episode, (start, end)))
            .collect(),
        end - start,
    ));
    for reference in &e[1..] {
        context
            .references
            .insert(reference.sample_id.clone(), reference.clone());
    }
    context
}

#[test]
fn proven_suffix_outside_context_does_not_require_whole_window_pcm() {
    let e = inputs(100_000);
    assert!(!complete_pcm(&e[0].capture));
    assert!(complete_guards(
        &e[0].capture,
        Interval {
            start_ms: 30_000,
            end_ms: 90_000
        },
        SegmentKind::Intro,
        e[0].duration_ms,
        &BoundaryPolicy::default()
    ));
}

#[test]
fn fullwindow_and_adaptive_review_accept_proven_local_target_and_reference_coverage() {
    let e = inputs(179_775);
    let policy = SamplingPolicy::default();
    let matcher = engine(30.0, 90.0);
    let decisions = verify_season_fingerprints_with(&matcher, &e, &Default::default(), &policy);
    assert!(
        decisions
            .iter()
            .all(|d| matches!(d.decision, FingerprintDecision::Verified(_))),
        "{decisions:?}"
    );
    let result = verify_template_window(&matcher, &e[0], &templates(&e, 30_000, 90_000), &policy);
    assert!(
        matches!(result, VerificationOutcome::Verified(_)),
        "{result:?}"
    );
}

#[test]
fn unknown_alignment_leading_loss_and_missing_required_guard_stay_uncertain() {
    let matcher = engine(30.0, 90.0);
    for (origin, pcm) in [
        (None, 179_775),
        (Some(5_000), 175_000),
        (Some(5_000), 180_000),
        (Some(-1), 179_775),
        (Some(0), 92_979),
    ] {
        let mut e = inputs(pcm);
        for sample in &mut e {
            set_proven_origin(sample, origin);
        }
        let decisions = verify_season_fingerprints_with(
            &matcher,
            &e,
            &Default::default(),
            &SamplingPolicy::default(),
        );
        assert!(
            decisions
                .iter()
                .all(|d| matches!(d.decision, FingerprintDecision::Uncertain(_))),
            "{decisions:?}"
        );
        let result = verify_template_window(
            &matcher,
            &e[0],
            &templates(&e, 30_000, 90_000),
            &SamplingPolicy::default(),
        );
        assert!(
            matches!(result, VerificationOutcome::NeedsFullWindow { .. }),
            "{result:?}"
        );
    }
}

#[test]
fn source_pts_alone_does_not_prove_a_short_capture_origin() {
    let mut e = inputs(179_775);
    for sample in &mut e {
        set_proven_origin(sample, None);
        sample.capture.metrics.source_start_pts_ms = Some(0);
    }
    let outcomes = verify_season_fingerprints_with(
        &engine(30.0, 90.0),
        &e,
        &Default::default(),
        &SamplingPolicy::default(),
    );
    assert!(
        outcomes
            .iter()
            .all(|d| matches!(d.decision, FingerprintDecision::Uncertain(_)))
    );
}

#[test]
fn local_coverage_never_relaxes_fullwindow_absence() {
    let e = inputs(179_775);
    let matcher = MockVerificationEngine::default();
    let decisions = verify_season_fingerprints_with(
        &matcher,
        &e,
        &Default::default(),
        &SamplingPolicy::default(),
    );
    assert!(
        decisions
            .iter()
            .all(|d| matches!(d.decision, FingerprintDecision::Uncertain(_)))
    );
    assert!(matches!(
        verify_template_window(
            &matcher,
            &e[0],
            &templates(&e, 30_000, 90_000),
            &SamplingPolicy::default()
        ),
        VerificationOutcome::NeedsFullWindow { .. }
    ));
}

#[test]
fn adaptive_near_media_start_uses_the_same_bounded_guard_geometry() {
    let e = inputs(180_000);
    let result = verify_template_window(
        &engine(0.123, 90.0),
        &e[0],
        &templates(&e, 123, 90_000),
        &SamplingPolicy::default(),
    );
    assert!(
        matches!(result, VerificationOutcome::Verified(_)),
        "{result:?}"
    );
}
