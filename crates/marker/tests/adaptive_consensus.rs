//! Consensus clustering, ordering and multi-template aggregation behavior of adaptive
//! template verification.

mod common;

use common::{
    common_segment, intro_template_model, make_target_evidence, template_reference,
    MockVerificationEngine,
};
use marker::{
    CommonSegment, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel, TemplateReference,
    VerificationOutcome, verify_template_window,
};

#[test]
fn multiple_template_models_tries_second_model_when_first_fails() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    // Model 1: length 40s (will fail)
    let model1 = TemplateModel {
        model_id: "intro-40-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "ledger-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 45_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "ledger-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 45_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 40_000,
        is_stable: true,
    };

    // Model 2: length 90s (will succeed)
    let model2 = TemplateModel {
        model_id: "intro-90-1".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "ref-s3".to_string(),
                ledger_id: "ledger-3".to_string(),
                episode: 3,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 95_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s4".to_string(),
                ledger_id: "ledger-4".to_string(),
                episode: 4,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 95_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut templates = TemplateContext::default();
    templates.models.push(model1);
    templates.models.push(model2);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s3".to_string(),
        make_target_evidence("ref-s3", 3, SegmentKind::Intro, 3000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s4".to_string(),
        make_target_evidence("ref-s4", 4, SegmentKind::Intro, 4000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 5, SegmentKind::Intro, 5000, 0, 180_000);

    // Model 1 returns no matches.
    // Model 2 returns valid 90s matches.
    let seg1 = CommonSegment {
        start1_sec: 5.0,
        end1_sec: 95.0,
        start2_sec: 5.0,
        end2_sec: 95.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    let seg2 = CommonSegment {
        start1_sec: 5.0,
        end1_sec: 95.0,
        start2_sec: 5.0,
        end2_sec: 95.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    engine.returns.insert((5000, 3000), vec![seg1]);
    engine.returns.insert((5000, 4000), vec![seg2]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::Verified(..)),
        "Should succeed on the second matching model, got: {:?}",
        outcome
    );
}

#[test]
fn conflicting_third_reference_excluded_from_support_and_order_independent() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let mut templates = TemplateContext::default();
    let model = TemplateModel {
        model_id: "tpl-1".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        expected_duration_ms: 90_000,
        is_stable: true,
        references: vec![
            TemplateReference {
                sample_id: "ref-s1".to_string(),
                ledger_id: "led-1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 95_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s2".to_string(),
                ledger_id: "led-2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 95_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "ref-s3".to_string(),
                ledger_id: "led-3".to_string(),
                episode: 3,
                source_version: "v1".to_string(),
                match_interval_ms: (5_000, 95_000),
                match_from_end_ms: None,
            },
        ],
    };
    templates.models.push(model);
    templates.references.insert(
        "ref-s1".to_string(),
        make_target_evidence("ref-s1", 1, SegmentKind::Intro, 1000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s2".to_string(),
        make_target_evidence("ref-s2", 2, SegmentKind::Intro, 2000, 0, 180_000),
    );
    templates.references.insert(
        "ref-s3".to_string(),
        make_target_evidence("ref-s3", 3, SegmentKind::Intro, 3000, 0, 180_000),
    );

    let target = make_target_evidence("target-s", 4, SegmentKind::Intro, 4000, 0, 180_000);

    // Ref 1 agrees with Ref 2: target [5.0s, 95.0s]
    let seg1 = CommonSegment {
        start1_sec: 5.0,
        end1_sec: 95.0,
        start2_sec: 5.0,
        end2_sec: 95.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    let seg2 = CommonSegment {
        start1_sec: 5.0,
        end1_sec: 95.0,
        start2_sec: 5.0,
        end2_sec: 95.0,
        duration_sec: 90.0,
        score: 1.0,
    };
    // Ref 3 is conflicting on target: target [40.0s, 130.0s] (outside tolerance delta)
    let seg3 = CommonSegment {
        start1_sec: 40.0,
        end1_sec: 130.0,
        start2_sec: 5.0,
        end2_sec: 95.0,
        duration_sec: 90.0,
        score: 1.0,
    };

    engine.returns.insert((4000, 1000), vec![seg1]);
    engine.returns.insert((4000, 2000), vec![seg2]);
    engine.returns.insert((4000, 3000), vec![seg3]);

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    match outcome {
        VerificationOutcome::Verified(v) => {
            // Ref 3 was conflicting, so supporting_episodes must be 2, NOT 3!
            assert_eq!(
                v.supporting_episodes, 2,
                "Conflicting reference should not be counted"
            );
            assert_eq!(v.start_ms, 5_000);
            assert_eq!(v.end_ms, 95_000);
        }
        other => panic!("Expected verified with 2 supporting episodes, got: {:?}", other),
    }

    // Now reverse the model references order so the conflicting one is first
    templates.models[0].references.reverse();
    let outcome_reversed = verify_template_window(&engine, &target, &templates, &policy);
    match outcome_reversed {
        VerificationOutcome::Verified(v) => {
            assert_eq!(
                v.supporting_episodes, 2,
                "Order must not affect consensus outcome"
            );
            assert_eq!(v.start_ms, 5_000);
            assert_eq!(v.end_ms, 95_000);
        }
        other => panic!("Expected verified regardless of order, got: {:?}", other),
    }
}

#[test]
fn review_greedy_clusters_cannot_hide_best_valid_five_reference_consensus() {
    let mut engine = MockVerificationEngine::default();
    let policy = SamplingPolicy::default();

    let mut references = Vec::new();
    let mut templates = TemplateContext::default();
    for ep in 1..=5 {
        let sid = format!("review-ref-{ep}");
        templates.references.insert(
            sid.clone(),
            make_target_evidence(&sid, ep, SegmentKind::Intro, ep * 1000, 0, 180_000),
        );
        references.push(TemplateReference {
            sample_id: sid,
            ledger_id: format!("ledger-{ep}"),
            episode: ep,
            source_version: "v1".to_string(),
            match_interval_ms: (10_000, 100_000),
            match_from_end_ms: None,
        });
    }

    let model = TemplateModel {
        model_id: "intro-5ref".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references,
        expected_duration_ms: 90_000,
        is_stable: true,
    };
    templates.models.push(model);

    let target = make_target_evidence("target", 9, SegmentKind::Intro, 9000, 0, 110_000);

    for (tag, start, score) in [
        (1000, 3.1, 1.0),
        (2000, 3.3, 1.0),
        (3000, 3.9, 1.0),
        (4000, 2.6, 4.0),
        (5000, 4.3, 4.0),
    ] {
        let seg = CommonSegment {
            start1_sec: start,
            end1_sec: start + 90.0,
            start2_sec: 10.0,
            end2_sec: 100.0,
            duration_sec: 90.0,
            score,
        };
        engine.returns.insert((9000, tag), vec![seg]);
    }

    templates.models[0].references.rotate_right(2);
    let high_score_first = verify_template_window(&engine, &target, &templates, &policy);
    templates.models[0].references.rotate_left(2);
    let low_score_first = verify_template_window(&engine, &target, &templates, &policy);

    assert!(matches!(high_score_first, VerificationOutcome::Verified(ref v) if v.start_ms == 3100 && v.end_ms == 93900 && v.supporting_episodes == 3 && (v.score - 1.0).abs() < 1e-6));
    assert!(matches!(low_score_first, VerificationOutcome::Verified(ref v) if v.start_ms == 3100 && v.end_ms == 93900 && v.supporting_episodes == 3 && (v.score - 1.0).abs() < 1e-6));
}

/// One template cannot align two independent references, another template has no candidate at
/// all: the inconclusive result must be preserved instead of being overwritten by the
/// empty-engine template, regardless of the template order.
#[test]
fn multi_template_inconclusive_result_survives_other_template_zero_candidates() {
    let (engine, templates, target) = multi_template_fixture([false, true]);
    let policy = SamplingPolicy::default();

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    match outcome {
        VerificationOutcome::NeedsFullWindow { reason } => {
            assert!(
                reason.contains("insufficient_reference_matches"),
                "inconclusive template reason must survive aggregation, got: {reason}"
            );
            assert!(
                reason.contains("zero_engine_segments"),
                "empty-engine template reason must be reported too, got: {reason}"
            );
        }
        other => panic!(
            "An inconclusive template plus an empty-engine template must require the full window, got: {:?}",
            other
        ),
    }
}

#[test]
fn multi_template_failure_reason_is_order_independent() {
    let (engine, templates, target) = multi_template_fixture([true, false]);
    let policy = SamplingPolicy::default();

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    match outcome {
        VerificationOutcome::NeedsFullWindow { reason } => {
            assert!(
                reason.contains("insufficient_reference_matches"),
                "reversed template order must keep the inconclusive reason, got: {reason}"
            );
            assert!(
                reason.contains("zero_engine_segments"),
                "reversed template order must keep the empty-engine reason, got: {reason}"
            );
        }
        other => panic!(
            "Reversing template order must not change the outcome, got: {:?}",
            other
        ),
    }
}

/// Only when every applicable template completed its comparison with no candidate may the
/// verification report a successful no-match.
#[test]
fn multi_template_all_zero_candidates_reports_no_match() {
    let (engine, templates, target) = multi_template_fixture([true, true]);
    let policy = SamplingPolicy::default();

    let outcome = verify_template_window(&engine, &target, &templates, &policy);
    assert!(
        matches!(outcome, VerificationOutcome::NoMatch { .. }),
        "All templates without candidates on a complete full window must report NoMatch, got: {:?}",
        outcome
    );
}

/// Two intro models on a complete 180s PCM window. `zero_engine` selects, per model in model
/// order, whether the model's references produce no engine segments at all; the other model
/// always aligns exactly one of its two references and stays inconclusive.
fn multi_template_fixture(zero_engine: [bool; 2]) -> (MockVerificationEngine, TemplateContext, marker::EpisodeEvidence) {
    let mut engine = MockVerificationEngine::default();
    let mut templates = TemplateContext::default();

    for (index, no_candidates) in zero_engine.iter().enumerate() {
        let first_episode = (index as u32) * 2 + 1;
        let second_episode = first_episode + 1;
        let first = format!("ref-{index}-a");
        let second = format!("ref-{index}-b");

        templates.models.push(intro_template_model(
            &format!("intro-model-{index}"),
            vec![
                template_reference(&first, first_episode, (10_000, 100_000)),
                template_reference(&second, second_episode, (10_000, 100_000)),
            ],
            90_000,
        ));

        let first_tag = 1000 + (index as u32) * 2000;
        let second_tag = first_tag + 1000;
        templates.references.insert(
            first.clone(),
            make_target_evidence(&first, first_episode, SegmentKind::Intro, first_tag, 0, 180_000),
        );
        templates.references.insert(
            second.clone(),
            make_target_evidence(
                &second,
                second_episode,
                SegmentKind::Intro,
                second_tag,
                0,
                180_000,
            ),
        );

        if !no_candidates {
            // First reference aligns with the template interval; the second one produces a
            // segment whose reference coordinates sit outside it, so it is not a candidate.
            engine.returns.insert(
                (9000, first_tag),
                vec![common_segment((10.0, 100.0), (10.0, 100.0), 1.0)],
            );
            engine.returns.insert(
                (9000, second_tag),
                vec![common_segment((10.0, 100.0), (30.0, 120.0), 1.0)],
            );
        }
    }

    let target = make_target_evidence("target", 9, SegmentKind::Intro, 9000, 0, 180_000);
    (engine, templates, target)
}
