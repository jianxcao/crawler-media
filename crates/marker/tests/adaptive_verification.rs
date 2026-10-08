use std::collections::HashMap;
use std::path::Path;

use marker::{
    CaptureMetrics, CapturedFingerprint, CommonSegment, EpisodeEvidence, FingerprintEngine,
    SampleWindow, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel, TemplateReference,
    VerificationOutcome, verify_template_window,
};

#[derive(Clone, Default)]
struct MockVerificationEngine {
    // Return segments based on key
    pub returns: HashMap<(u32, u32), Vec<CommonSegment>>,
}

impl FingerprintEngine for MockVerificationEngine {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
    ) -> Result<Vec<u32>, String> {
        Ok(Vec::new())
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        self.find_common_segments(first, second, min_duration_secs, max_duration_secs)
            .into_iter()
            .next()
    }

    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Vec<CommonSegment> {
        let tag1 = first.first().copied().unwrap_or(0);
        let tag2 = second.first().copied().unwrap_or(0);
        if let Some(res) = self.returns.get(&(tag1, tag2)) {
            return res.clone();
        }
        if let Some(res) = self.returns.get(&(tag2, tag1)) {
            // Inverted
            return res
                .iter()
                .map(|s| CommonSegment {
                    start1_sec: s.start2_sec,
                    end1_sec: s.end2_sec,
                    start2_sec: s.start1_sec,
                    end2_sec: s.end1_sec,
                    duration_sec: s.duration_sec,
                    score: s.score,
                })
                .collect();
        }
        Vec::new()
    }
}

fn make_target_evidence(
    sample_id: &str,
    episode: u32,
    kind: SegmentKind,
    tag: u32,
    window_start_ms: i64,
    window_end_ms: i64,
) -> EpisodeEvidence {
    let len = 500;
    let mut words = Vec::with_capacity(len);
    for i in 0..len {
        words.push(tag + i as u32);
    }
    EpisodeEvidence {
        sample_id: sample_id.to_string(),
        ledger_id: format!("ledger-{}", episode),
        episode,
        source_version: "v1".to_string(),
        capture_profile_key: "default".to_string(),
        kind,
        capture: CapturedFingerprint {
            window: SampleWindow {
                start_ms: window_start_ms,
                end_ms: window_end_ms,
            },
            words,
            pcm_duration_ms: Some(window_end_ms - window_start_ms),
            metrics: CaptureMetrics {
                elapsed_ms: 100,
                time_to_first_pcm_ms: Some(50),
                pcm_read_wait_us: 10,
                chromaprint_consume_us: 10,
                pcm_bytes: 5000,
                input_bytes: None,
                input_bytes_source: None,
                measurement_complete: true,
                ffmpeg_exit_code: Some(0),
                command_setup_ms: None,
                ffmpeg_spawn_ms: None,
                pcm_stream_elapsed_ms: None,
                chromaprint_finish_ms: None,
                ffmpeg_wait_ms: None,
                stderr_collect_ms: None,
                sample_count: None,
                ffmpeg_user_cpu_ms: None,
                ffmpeg_system_cpu_ms: None,
                ffmpeg_real_ms: None,
                ffmpeg_maxrss_kb: None,
                ffmpeg_stderr_bytes: None,
                ffmpeg_stderr_tail: None,
            },
        },
        duration_ms: Some(1_200_000),
    }
}

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
            assert_eq!(v.supporting_episodes, 2, "Conflicting reference should not be counted");
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
            assert_eq!(v.supporting_episodes, 2, "Order must not affect consensus outcome");
            assert_eq!(v.start_ms, 5_000);
            assert_eq!(v.end_ms, 95_000);
        }
        other => panic!("Expected verified regardless of order, got: {:?}", other),
    }
}
