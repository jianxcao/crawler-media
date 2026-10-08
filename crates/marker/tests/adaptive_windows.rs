use marker::{
    EpisodeDescriptor, SamplingPolicy, SegmentKind, SourceCostSummary,
    TemplateContext, TemplateModel, TemplateReference, WindowDecision, plan_episode_window,
};

#[test]
fn outro_prediction_uses_target_duration() {
    // Model member episodes had duration 1_000_000 ms, outro from end: [100_000, 10_000] (i.e. length 90_000ms)
    // Target episode has duration 1_500_000 ms.
    // Prediction must place outro around 1_400_000 to 1_490_000, NOT 900_000!
    let policy = SamplingPolicy::default();
    let template = TemplateModel {
        model_id: "outro-90-0".to_string(),
        version: 1,
        kind: SegmentKind::Outro,
        references: vec![
            TemplateReference {
                sample_id: "s1".to_string(),
                ledger_id: "l1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (900_000, 990_000),
                match_from_end_ms: Some((100_000, 10_000)),
            },
            TemplateReference {
                sample_id: "s2".to_string(),
                ledger_id: "l2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (900_000, 990_000),
                match_from_end_ms: Some((100_000, 10_000)),
            },
        ],
        expected_duration_ms: 90_000,
        is_stable: true,
    };

    let mut context = TemplateContext::default();
    context.models.push(template);

    let target = EpisodeDescriptor {
        ledger_id: "l-target".to_string(),
        episode: 3,
        source_version: "v1".to_string(),
        duration_ms: 1_500_000,
    };

    let cost = SourceCostSummary::default();
    let decision = plan_episode_window(&target, SegmentKind::Outro, &context, &policy, &cost);

    match decision {
        WindowDecision::Verify { window, .. } => {
            // Predicted start: 1_500_000 - 100_000 - 10_000 (margin) = 1_390_000
            // Predicted end: 1_500_000 - 10_000 + 10_000 (margin) = 1_500_000
            assert!(
                window.start_ms >= 1_380_000 && window.start_ms <= 1_400_000,
                "Start should be relative to target duration (around 1_390_000), got {}",
                window.start_ms
            );
            assert_eq!(window.end_ms, 1_500_000);
        }
        WindowDecision::Full { reason, .. } => {
            panic!("Expected WindowDecision::Verify, got Full: {reason}");
        }
    }
}

#[test]
fn close_candidates_share_one_contiguous_window() {
    let policy = SamplingPolicy::default();
    // Two template candidates close to each other: [30_000, 60_000] and [50_000, 90_000]
    let template1 = TemplateModel {
        model_id: "intro-30-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "s1".to_string(),
                ledger_id: "l1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (30_000, 60_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "s2".to_string(),
                ledger_id: "l2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (30_000, 60_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 30_000,
        is_stable: true,
    };
    let template2 = TemplateModel {
        model_id: "intro-40-1".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "s3".to_string(),
                ledger_id: "l3".to_string(),
                episode: 3,
                source_version: "v1".to_string(),
                match_interval_ms: (50_000, 90_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "s4".to_string(),
                ledger_id: "l4".to_string(),
                episode: 4,
                source_version: "v1".to_string(),
                match_interval_ms: (50_000, 90_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 40_000,
        is_stable: true,
    };

    let mut context = TemplateContext::default();
    context.models.push(template1);
    context.models.push(template2);

    let target = EpisodeDescriptor {
        ledger_id: "l-target".to_string(),
        episode: 5,
        source_version: "v1".to_string(),
        duration_ms: 1_200_000,
    };

    let cost = SourceCostSummary::default();
    let decision = plan_episode_window(&target, SegmentKind::Intro, &context, &policy, &cost);

    match decision {
        WindowDecision::Verify { window, model_ids } => {
            // Context margin is 10s.
            // min_start = 30_000 - 10_000 = 20_000
            // max_end = 90_000 + 10_000 = 100_000
            assert_eq!(window.start_ms, 20_000);
            assert_eq!(window.end_ms, 100_000);
            assert_eq!(model_ids.len(), 2);
        }
        WindowDecision::Full { reason, .. } => {
            panic!("Expected WindowDecision::Verify, got Full: {reason}");
        }
    }
}

#[test]
fn large_prediction_skips_fast_path() {
    let policy = SamplingPolicy::default();
    // A huge template: [10_000, 175_000] in a 180_000 full window
    let template = TemplateModel {
        model_id: "intro-165-0".to_string(),
        version: 1,
        kind: SegmentKind::Intro,
        references: vec![
            TemplateReference {
                sample_id: "s1".to_string(),
                ledger_id: "l1".to_string(),
                episode: 1,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 175_000),
                match_from_end_ms: None,
            },
            TemplateReference {
                sample_id: "s2".to_string(),
                ledger_id: "l2".to_string(),
                episode: 2,
                source_version: "v1".to_string(),
                match_interval_ms: (10_000, 175_000),
                match_from_end_ms: None,
            },
        ],
        expected_duration_ms: 165_000,
        is_stable: true,
    };

    let mut context = TemplateContext::default();
    context.models.push(template);

    let target = EpisodeDescriptor {
        ledger_id: "l-target".to_string(),
        episode: 3,
        source_version: "v1".to_string(),
        duration_ms: 1_200_000,
    };

    let cost = SourceCostSummary::default();
    let decision = plan_episode_window(&target, SegmentKind::Intro, &context, &policy, &cost);

    match decision {
        WindowDecision::Full { reason, .. } => {
            assert!(
                reason.contains("window_saving_below_threshold")
                    || reason.contains("cost_saving_below_threshold"),
                "Expected saving below threshold, got: {reason}"
            );
        }
        WindowDecision::Verify { window, .. } => {
            panic!("Expected Full window for large prediction, got Verify: {:?}", window);
        }
    }
}
