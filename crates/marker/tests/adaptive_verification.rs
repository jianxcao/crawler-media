//! Window/guard boundary behavior of adaptive template verification.

mod common;

use common::{make_target_evidence, MockVerificationEngine};
use marker::{
    CommonSegment, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel, TemplateReference,
    VerificationOutcome, verify_template_window,
};

#[test]
fn center_only_match_requires_full_window() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let template = TemplateModel {
        model_id: "intro-90-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (0, 90_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (0, 90_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(template);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 3, SegmentKind::Intro, 3000, 0, 100_000);

    let center_seg = CommonSegment {
        start1_sec: 35.0,
        end1_sec: 55.0,
        start2_sec: 35.0,
        end2_sec: 55.0,
        duration_sec: 20.0,
        score: 1.0,
    };
    engine.returns.insert((3000, 1000), vec![center_seg.clone()]);
    engine.returns.insert((3000, 2000), vec![center_seg]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::NeedsFullWindow { .. }),
        "Expected NeedsFullWindow for center-only match, got: {:?}",
        outcome
    );
}

#[test]
fn intro_zero_start_is_valid_with_other_boundary_evidence() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let template = TemplateModel {
        model_id: "intro-90-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (0, 90_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (0, 90_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(template);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 3, SegmentKind::Intro, 3000, 0, 100_000);

    let full_seg = CommonSegment {
        start1_sec: 0.0,
        end1_sec: 90.0,
        start2_sec: 0.0,
        end2_sec: 90.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    engine.returns.insert((3000, 1000), vec![full_seg.clone()]);
    engine.returns.insert((3000, 2000), vec![full_seg]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    match outcome {
        VerificationOutcome::Verified(verified) => {
            assert_eq!(verified.start_ms, 0);
            assert_eq!(verified.end_ms, 90_000);
            assert_eq!(verified.supporting_episodes, 2);
        }
        _ => panic!("Expected Verified, got {:?}", outcome),
    }
}

#[test]
fn window_edge_match_is_not_a_complete_template() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let template = TemplateModel {
        model_id: "intro-90-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 100_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 100_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(template);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 3, SegmentKind::Intro, 3000, 10_000, 95_000);

    let clipped_seg = CommonSegment {
        start1_sec: 0.0,
        end1_sec: 85.0,
        start2_sec: 0.0,
        end2_sec: 85.0,
        duration_sec: 85.0,
        score: 1.0,
    };
    engine.returns.insert((3000, 1000), vec![clipped_seg.clone()]);
    engine.returns.insert((3000, 2000), vec![clipped_seg]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::NeedsFullWindow { .. }),
        "Expected NeedsFullWindow when edge clipped, got: {:?}",
        outcome
    );
}

#[test]
fn edited_middle_and_conflicting_references_require_fallback() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let template = TemplateModel {
        model_id: "intro-90-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 100_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 100_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(template);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 3, SegmentKind::Intro, 3000, 0, 120_000);

    let seg1 = CommonSegment {
        start1_sec: 10.0,
        end1_sec: 100.0,
        start2_sec: 10.0,
        end2_sec: 100.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    let seg2 = CommonSegment {
        start1_sec: 15.0,
        end1_sec: 105.0,
        start2_sec: 10.0,
        end2_sec: 100.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    engine.returns.insert((3000, 1000), vec![seg1]);
    engine.returns.insert((3000, 2000), vec![seg2]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::NeedsFullWindow { .. }),
        "Expected NeedsFullWindow for conflicting reference delta > 1000ms, got: {:?}",
        outcome
    );
}

#[test]
fn match_outside_reference_interval_requires_full_window() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let template = TemplateModel {
        model_id: "intro-90-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (0, 90_000), // Template interval is [0, 90s]
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (0, 90_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(template);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 3, SegmentKind::Intro, 3000, 0, 180_000);

    // Matches occur at [90s, 180s] in the reference, outside the [0, 90s] template interval
    let bad_seg1 = CommonSegment {
        start1_sec: 10.0,
        end1_sec: 100.0,
        start2_sec: 90.0,
        end2_sec: 180.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    let bad_seg2 = CommonSegment {
        start1_sec: 10.0,
        end1_sec: 100.0,
        start2_sec: 90.0,
        end2_sec: 180.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    engine.returns.insert((3000, 1000), vec![bad_seg1]);
    engine.returns.insert((3000, 2000), vec![bad_seg2]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::NeedsFullWindow { .. }),
        "Matches outside reference interval must require full window fallback, got: {:?}",
        outcome
    );
}

#[test]
fn truncated_pcm_duration_prevents_fake_guard_evidence() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let template = TemplateModel {
        model_id: "intro-50-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 55_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 55_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 50_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(template);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 110_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 110_000),
    );

    // Target requested [0, 110s], but actual decoded PCM is only 55s!
    let mut target = make_target_evidence("target-s", 3, SegmentKind::Intro, 3000, 0, 110_000);
    target.capture.pcm_duration_ms = Some(55_000);

    let seg1 = CommonSegment {
        start1_sec: 5.0,
        end1_sec: 55.0,
        start2_sec: 5.0,
        end2_sec: 55.0,
        duration_sec: 50.0,
        score: 1.0,
    };
    let seg2 = CommonSegment {
        start1_sec: 5.0,
        end1_sec: 55.0,
        start2_sec: 5.0,
        end2_sec: 55.0,
        duration_sec: 50.0,
        score: 1.0,
    };
    engine.returns.insert((3000, 1000), vec![seg1]);
    engine.returns.insert((3000, 2000), vec![seg2]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::NeedsFullWindow { .. }),
        "Truncated decoded PCM with zero right-guard must fail boundary checks, got: {:?}",
        outcome
    );
}

#[test]
fn review_known_complete_full_window_zero_candidates_reports_no_match() {
    let engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let mut templates = TemplateContext::default();
    let template = TemplateModel {
        model_id: "intro-nomatch".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-1".to_string(),
                ledger_id: "l-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 100_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-2".to_string(),
                ledger_id: "l-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 100_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };
    templates.models.push(template);
    templates.references.insert(
        "ref-1".to_string(),
        make_target_evidence("ref-1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-2".to_string(),
        make_target_evidence("ref-2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );

    let target = make_target_evidence("target", 9, SegmentKind::Intro, 9000, 0, 180_000);
    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(matches!(outcome, VerificationOutcome::NoMatch { .. }));
}
