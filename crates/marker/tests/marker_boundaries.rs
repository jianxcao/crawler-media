use marker::SampleWindow;
use marker::boundary::{
    BoundaryDecision, BoundaryEvidence, BoundaryPolicy, Interval, normalize_interval,
    refine_boundary,
};

fn evidence() -> BoundaryEvidence {
    BoundaryEvidence {
        candidate: Interval {
            start_ms: 30_000,
            end_ms: 90_000,
        },
        complete_pcm: true,
        complete_context: true,
        chapter: None,
        audio_transition: None,
        references_agree: true,
        complete_guards: true,
        conflicting_evidence: false,
    }
}
#[test]
fn outro_uses_actual_capture_start_not_fractional_container_duration() {
    let window = SampleWindow::new(1_620_000, 1_800_000);
    assert_eq!(
        normalize_interval(
            &window,
            Interval {
                start_ms: 5_000,
                end_ms: 65_000
            }
        )
        .unwrap(),
        Interval {
            start_ms: 1_625_000,
            end_ms: 1_685_000
        }
    );
}
#[test]
fn normalization_rejects_invalid_and_overflowing_ranges() {
    for relative in [
        Interval {
            start_ms: -1,
            end_ms: 20,
        },
        Interval {
            start_ms: 20,
            end_ms: 10,
        },
        Interval {
            start_ms: 0,
            end_ms: 101,
        },
    ] {
        assert!(normalize_interval(&SampleWindow::new(10, 110), relative).is_err());
    }
    assert!(
        normalize_interval(
            &SampleWindow::new(i64::MAX - 100, i64::MAX),
            Interval {
                start_ms: 0,
                end_ms: 101
            }
        )
        .is_err()
    );
}
#[test]
fn refinement_never_expands_a_candidate() {
    let mut e = evidence();
    e.chapter = Some(Interval {
        start_ms: 20_000,
        end_ms: 90_000,
    });
    e.audio_transition = e.chapter;
    assert!(matches!(
        refine_boundary(&e, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
}
#[test]
fn agreeing_auxiliary_evidence_selects_common_interior() {
    let mut e = evidence();
    e.chapter = Some(Interval {
        start_ms: 30_500,
        end_ms: 89_900,
    });
    e.audio_transition = Some(Interval {
        start_ms: 30_800,
        end_ms: 90_000,
    });
    assert_eq!(
        refine_boundary(&e, &BoundaryPolicy::default()),
        BoundaryDecision::Verified(Interval {
            start_ms: 30_800,
            end_ms: 89_900
        })
    );
}
#[test]
fn complete_guards_are_required_without_auxiliary_evidence() {
    let mut e = evidence();
    assert_eq!(
        refine_boundary(&e, &BoundaryPolicy::default()),
        BoundaryDecision::Verified(e.candidate)
    );
    e.complete_guards = false;
    assert!(matches!(
        refine_boundary(&e, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
    e.complete_guards = true;
    e.conflicting_evidence = true;
    assert!(matches!(
        refine_boundary(&e, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
}
#[test]
fn bounded_audio_context_retains_observations_without_semantic_veto() {
    use marker::boundary::evidence::collect_boundary_evidence;
    use marker::{CaptureMetrics, CapturedFingerprint, SegmentKind};
    let mut frames = vec![-20_000; 6000];
    frames[1400..1500].fill(-100_000);
    frames[4500..4600].fill(-100_000);
    let mut capture = CapturedFingerprint {
        window: SampleWindow::new(0, 120_000),
        words: vec![1, 2],
        pcm_duration_ms: Some(120_000),
        metrics: CaptureMetrics {
            pcm_frame_ms: 20,
            pcm_rms_dbfs_milli: frames,
            ..Default::default()
        },
    };
    let candidate = Interval {
        start_ms: 30_000,
        end_ms: 90_000,
    };
    let e = collect_boundary_evidence(
        &capture,
        candidate,
        &[],
        SegmentKind::Intro,
        Some(120_000),
        marker::boundary::evidence::ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert!(
        !e.conflicting_evidence,
        "separated raw silence observations do not prove semantic conflict"
    );
    capture.metrics.pcm_rms_dbfs_milli.fill(-20_000);
    let e = collect_boundary_evidence(
        &capture,
        candidate,
        &[],
        SegmentKind::Intro,
        Some(120_000),
        marker::boundary::evidence::ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert!(!e.conflicting_evidence);
    assert!(e.complete_guards);
    capture.pcm_duration_ms = Some(110_000);
    let e = collect_boundary_evidence(
        &capture,
        candidate,
        &[],
        SegmentKind::Intro,
        Some(120_000),
        marker::boundary::evidence::ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert!(!e.complete_pcm);
    assert!(!e.complete_guards);
}
#[test]
fn natural_media_edges_require_checked_origins_and_duration() {
    use marker::boundary::evidence::complete_guards;
    use marker::{CaptureMetrics, CapturedFingerprint, SegmentKind};
    let mut capture = CapturedFingerprint {
        window: SampleWindow::new(0, 60_000),
        words: vec![],
        pcm_duration_ms: Some(60_000),
        metrics: CaptureMetrics::default(),
    };
    let policy = BoundaryPolicy::default();
    assert!(complete_guards(
        &capture,
        Interval {
            start_ms: 0,
            end_ms: 50_000
        },
        SegmentKind::Intro,
        Some(60_000),
        &policy
    ));
    assert!(complete_guards(
        &capture,
        Interval {
            start_ms: 0,
            end_ms: 60_000
        },
        SegmentKind::Intro,
        Some(60_000),
        &policy
    ));
    capture.window = SampleWindow::new(60_000, 120_000);
    assert!(complete_guards(
        &capture,
        Interval {
            start_ms: 70_000,
            end_ms: 120_000
        },
        SegmentKind::Outro,
        Some(120_000),
        &policy
    ));
    assert!(!complete_guards(
        &capture,
        Interval {
            start_ms: 70_000,
            end_ms: 120_000
        },
        SegmentKind::Outro,
        None,
        &policy
    ));
}

#[test]
fn independent_matches_do_not_replace_complete_reference_guards() {
    use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
    struct Matcher;
    impl FingerprintEngine for Matcher {
        fn extract_at(
            &self,
            _: &std::path::Path,
            _: u32,
            _: u32,
            _: &marker::WorkControl,
        ) -> Result<AudioFingerprint, String> {
            unreachable!()
        }
        fn find_common_segment(
            &self,
            _: &[u32],
            _: &[u32],
            _: f32,
            _: f32,
        ) -> Option<CommonSegment> {
            Some(CommonSegment {
                start1_sec: 30.0,
                end1_sec: 90.0,
                start2_sec: 30.0,
                end2_sec: 90.0,
                duration_sec: 60.0,
                score: 0.0,
            })
        }
    }
    let make = |episode| marker::EpisodeEvidence {
        sample_id: format!("sample-{episode}"),
        ledger_id: format!("file-{episode}"),
        episode,
        source_version: "v".into(),
        capture_profile_key: "profile".into(),
        kind: marker::SegmentKind::Intro,
        capture: marker::CapturedFingerprint {
            window: SampleWindow::new(0, 120_000),
            words: vec![1, 2, 3, 4, 5],
            pcm_duration_ms: Some(120_000),
            metrics: marker::CaptureMetrics::default(),
        },
        duration_ms: Some(120_000),
    };
    let target = make(1);
    let first = make(2);
    let mut second = make(3);
    let candidate = Interval {
        start_ms: 30_000,
        end_ms: 90_000,
    };
    let policy = marker::SamplingPolicy::default();
    assert_eq!(
        marker::boundary::evidence::review_candidate(
            &Matcher,
            &target,
            &[&first, &second],
            candidate,
            &[],
            &policy
        ),
        BoundaryDecision::Verified(candidate)
    );
    second.capture.window = SampleWindow::new(0, 90_000);
    second.capture.pcm_duration_ms = Some(90_000);
    assert!(matches!(
        marker::boundary::evidence::review_candidate(
            &Matcher,
            &target,
            &[&first, &second],
            candidate,
            &[],
            &policy
        ),
        BoundaryDecision::Uncertain(_)
    ));
    second = first.clone();
    assert!(matches!(
        marker::boundary::evidence::review_candidate(
            &Matcher,
            &target,
            &[&first, &second],
            candidate,
            &[],
            &policy
        ),
        BoundaryDecision::Uncertain(_)
    ));
}

#[test]
fn normal_decoder_overrun_does_not_make_missing_guard_evidence() {
    use marker::boundary::evidence::{complete_guards, complete_pcm};
    use marker::{CaptureMetrics, CapturedFingerprint, SegmentKind};
    let mut capture = CapturedFingerprint {
        window: SampleWindow::new(0, 120_000),
        words: vec![],
        pcm_duration_ms: Some(120_024),
        metrics: CaptureMetrics::default(),
    };
    assert!(complete_pcm(&capture));
    assert!(complete_guards(
        &capture,
        Interval {
            start_ms: 0,
            end_ms: 117_010
        },
        SegmentKind::Intro,
        Some(120_000),
        &BoundaryPolicy::default()
    ));
    capture.pcm_duration_ms = Some(119_940);
    assert!(!complete_pcm(&capture));
}

#[test]
fn unique_audio_and_source_chapter_corroboration_can_verify_a_real_candidate() {
    use marker::boundary::evidence::collect_boundary_evidence;
    let mut frames = vec![-20_000; 6000];
    frames[1490..1500].fill(-100_000);
    frames[4500..4510].fill(-100_000);
    let capture = marker::CapturedFingerprint {
        window: SampleWindow::new(0, 120_000),
        words: vec![1, 2, 3],
        pcm_duration_ms: Some(120_000),
        metrics: marker::CaptureMetrics {
            pcm_frame_ms: 20,
            pcm_rms_dbfs_milli: frames,
            ..Default::default()
        },
    };
    let candidate = Interval {
        start_ms: 29_000,
        end_ms: 91_000,
    };
    let actual = Interval {
        start_ms: 30_000,
        end_ms: 90_000,
    };
    let evidence = collect_boundary_evidence(
        &capture,
        candidate,
        &[actual],
        marker::SegmentKind::Intro,
        Some(120_000),
        marker::boundary::evidence::ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert_eq!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Verified(actual)
    );
}

#[test]
fn fullwindow_decisions_preserve_ambiguity_support_and_confirmed_absence() {
    use marker::matcher::{FingerprintDecision, verify_season_fingerprints_with};
    struct Engine(u8);
    impl marker::FingerprintEngine for Engine {
        fn compare_fingerprints(
            &self,
            first: &[u32],
            second: &[u32],
            min: f32,
            max: f32,
        ) -> marker::PairFingerprintDecision {
            let candidates = self.find_common_segments(first, second, min, max);
            if candidates.is_empty() {
                marker::PairFingerprintDecision::NoMatch
            } else {
                marker::PairFingerprintDecision::Candidates(candidates)
            }
        }

        fn extract_at(
            &self,
            _: &std::path::Path,
            _: u32,
            _: u32,
            _: &marker::WorkControl,
        ) -> Result<Vec<u32>, String> {
            unreachable!()
        }
        fn find_common_segment(
            &self,
            _: &[u32],
            _: &[u32],
            _: f32,
            _: f32,
        ) -> Option<marker::CommonSegment> {
            None
        }
        fn find_common_segments(
            &self,
            _: &[u32],
            _: &[u32],
            _: f32,
            _: f32,
        ) -> Vec<marker::CommonSegment> {
            if self.0 == 0 {
                return vec![];
            }
            let segment = |start| marker::CommonSegment {
                start1_sec: start,
                end1_sec: start + 60.0,
                start2_sec: start,
                end2_sec: start + 60.0,
                duration_sec: 60.0,
                score: if self.0 == 3 { 9.0 } else { 0.0 },
            };
            if self.0 == 2 {
                vec![segment(30.0), segment(110.0)]
            } else {
                vec![segment(30.0)]
            }
        }
    }
    let evidence = |episode| marker::EpisodeEvidence {
        sample_id: format!("s{episode}"),
        ledger_id: format!("f{episode}"),
        episode,
        source_version: "v1".into(),
        capture_profile_key: "p".into(),
        kind: marker::SegmentKind::Intro,
        capture: marker::CapturedFingerprint {
            window: SampleWindow::new(0, 180_000),
            words: vec![1, 2, 3],
            pcm_duration_ms: Some(180_000),
            metrics: marker::CaptureMetrics::default(),
        },
        duration_ms: Some(1_000_000),
    };
    let mut inputs = vec![evidence(1), evidence(2), evidence(3)];
    let policy = marker::SamplingPolicy::default();
    let chapters = std::collections::HashMap::new();
    for case in [0, 1, 2, 3] {
        let outcomes = verify_season_fingerprints_with(&Engine(case), &inputs, &chapters, &policy);
        assert_eq!(outcomes.len(), 3);
        for outcome in outcomes {
            assert_eq!(outcome.kind, marker::SegmentKind::Intro);
            assert_eq!(outcome.ledger_id, format!("f{}", outcome.episode));
            assert!(
                match case {
                    0 => matches!(outcome.decision, FingerprintDecision::NoMatch),
                    1 => matches!(outcome.decision, FingerprintDecision::Verified(_)),
                    _ => matches!(outcome.decision, FingerprintDecision::Uncertain(_)),
                },
                "case={case}: {outcome:?}"
            );
        }
    }
    inputs.pop();
    let outcomes = verify_season_fingerprints_with(&Engine(1), &inputs, &chapters, &policy);
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o.decision, FingerprintDecision::Uncertain(_)))
    );
    inputs.push(evidence(2));
    let outcomes = verify_season_fingerprints_with(&Engine(0), &inputs, &chapters, &policy);
    assert!(
        matches!(outcomes[0].decision, FingerprintDecision::Uncertain(_)),
        "duplicate reference versions cannot prove absence"
    );
    inputs[2] = evidence(3);
    inputs[0].capture.pcm_duration_ms = Some(179_500);
    let outcomes = verify_season_fingerprints_with(&Engine(0), &inputs, &chapters, &policy);
    assert!(
        matches!(outcomes[0].decision, FingerprintDecision::Uncertain(_)),
        "missing PCM cannot prove absence"
    );
}

#[test]
fn rejected_native_candidates_cannot_establish_fullwindow_absence() {
    use marker::matcher::{FingerprintDecision, verify_season_fingerprints_with};
    let mut state = 19u32;
    let words = (0..1400)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            state
        })
        .collect::<Vec<_>>();
    let make = |episode, mask| marker::EpisodeEvidence {
        sample_id: format!("s{episode}"),
        ledger_id: format!("f{episode}"),
        episode,
        source_version: "v1".into(),
        capture_profile_key: "p".into(),
        kind: marker::SegmentKind::Intro,
        capture: marker::CapturedFingerprint {
            window: SampleWindow::new(0, 180_000),
            words: words.iter().map(|w| w ^ mask).collect(),
            pcm_duration_ms: Some(180_000),
            metrics: marker::CaptureMetrics::default(),
        },
        duration_ms: Some(1_000_000),
    };
    let inputs = [make(1, 0), make(2, 0b11_1111), make(3, 0b11_1111)];
    let outcomes = verify_season_fingerprints_with(
        &marker::ChromaprintEngine,
        &inputs,
        &std::collections::HashMap::new(),
        &marker::SamplingPolicy::default(),
    );
    assert!(
        matches!(outcomes[0].decision, FingerprintDecision::Uncertain(_)),
        "native weak segments rejected internally must remain unresolved: {:?}",
        outcomes[0]
    );
}

#[test]
fn candidate_only_empty_engine_cannot_claim_completed_absence() {
    use marker::FingerprintEngine;
    struct Unknown;
    impl FingerprintEngine for Unknown {
        fn extract_at(
            &self,
            _: &std::path::Path,
            _: u32,
            _: u32,
            _: &marker::WorkControl,
        ) -> Result<Vec<u32>, String> {
            unreachable!()
        }
        fn find_common_segment(
            &self,
            _: &[u32],
            _: &[u32],
            _: f32,
            _: f32,
        ) -> Option<marker::CommonSegment> {
            None
        }
    }
    assert!(matches!(
        Unknown.compare_fingerprints(&[1, 2], &[3, 4], 15.0, 240.0),
        marker::PairFingerprintDecision::Uncertain(_)
    ));
    assert_eq!(
        marker::ChromaprintEngine.compare_fingerprints(
            &[1, 2],
            &[0xfff0_0003, 0xfff0_0004],
            15.0,
            240.0
        ),
        marker::PairFingerprintDecision::NoMatch
    );
}
