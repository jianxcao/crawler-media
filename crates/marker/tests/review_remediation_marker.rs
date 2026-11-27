//! Regression tests for boundary mapping skew/origin enforcement and EOF PCM absence proof.

use std::collections::HashMap;
use std::path::Path;

use marker::adaptive::{
    EpisodeEvidence, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel,
    TemplateReference, VerificationOutcome, verify_template_window,
};
use marker::boundary::evidence::{
    ReferenceSupport, collect_boundary_evidence, complete_eof_pcm, complete_pcm,
};
use marker::boundary::{BoundaryPolicy, Interval, normalize_capture_interval};
use marker::{
    CaptureMetrics, CapturedFingerprint, CommonSegment, FingerprintEngine, PairFingerprintDecision,
    PcmTimeline, SampleWindow, WorkControl,
};

struct EmptyMatcher;

impl FingerprintEngine for EmptyMatcher {
    fn extract_at(&self, _: &Path, _: u32, _: u32, _: &WorkControl) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(&self, _: &[u32], _: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        None
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> PairFingerprintDecision {
        PairFingerprintDecision::NoMatch
    }
}

#[test]
fn normalize_capture_interval_rejects_unproven_v3_and_out_of_range_legacy_skew() {
    let rel = Interval {
        start_ms: 10_000,
        end_ms: 40_000,
    };
    for (ver, timeline, origin) in [
        (3, PcmTimeline::Unknown, Some(0)),
        (3, PcmTimeline::Contiguous, None),
        (3, PcmTimeline::Contiguous, Some(-1)),
        (3, PcmTimeline::Contiguous, Some(51)),
        (2, PcmTimeline::Unknown, Some(-1)),
        (2, PcmTimeline::Unknown, Some(51)),
    ] {
        let cap = CapturedFingerprint {
            window: SampleWindow::new(0, 120_000),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(120_000),
            metrics: CaptureMetrics {
                time_mapping_version: ver,
                pcm_timeline: timeline,
                contiguous_pcm_start_ms: origin,
                ..Default::default()
            },
        };
        assert!(
            normalize_capture_interval(&cap, rel).is_err(),
            "ver={ver} origin={origin:?} must return Err"
        );
    }

    let legacy_none = CapturedFingerprint {
        window: SampleWindow::new(5_000, 125_000),
        words: vec![1, 2, 3, 4],
        pcm_duration_ms: Some(120_000),
        metrics: CaptureMetrics {
            time_mapping_version: 2,
            contiguous_pcm_start_ms: None,
            ..Default::default()
        },
    };
    assert_eq!(
        normalize_capture_interval(&legacy_none, rel).unwrap(),
        Interval {
            start_ms: 15_000,
            end_ms: 45_000,
        }
    );
}

#[test]
fn audio_boundaries_uses_proven_origin_without_double_applying_skew() {
    let mut frames = vec![-18_000i32; 6_000];
    frames[1_488..1_498].fill(-90_000); // second edge at 1498 * 20 = 29_960ms PCM -> 30_000ms media
    frames[4_488..4_498].fill(-90_000); // second edge at 4498 * 20 = 89_960ms PCM -> 90_000ms media
    let skewed = CapturedFingerprint {
        window: SampleWindow::new(0, 120_000),
        words: vec![1, 2, 3, 4],
        pcm_duration_ms: Some(119_960),
        metrics: CaptureMetrics {
            time_mapping_version: 3,
            pcm_timeline: PcmTimeline::Contiguous,
            contiguous_pcm_start_ms: Some(40),
            pcm_frame_ms: 20,
            pcm_rms_dbfs_milli: frames,
            ..Default::default()
        },
    };
    let ev = collect_boundary_evidence(
        &skewed,
        Interval {
            start_ms: 29_500,
            end_ms: 90_500,
        },
        &[],
        SegmentKind::Intro,
        Some(120_000),
        ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert_eq!(
        ev.audio_transition,
        Some(Interval {
            start_ms: 30_000,
            end_ms: 90_000
        }),
        "skewed v3 audio_boundaries must map to 30_000..90_000 without double-applying +40ms skew"
    );
}

#[test]
fn eof_pcm_shortfall_enables_adaptive_outro_no_match_while_keeping_non_eof_strict() {
    let mk_eof_outro = |sid: &str, ep: u32| EpisodeEvidence {
        sample_id: sid.into(),
        ledger_id: format!("f{ep}"),
        episode: ep,
        source_version: "v1".into(),
        capture_profile_key: "prof-v3".into(),
        kind: SegmentKind::Outro,
        duration_ms: Some(1_500_000),
        capture: CapturedFingerprint {
            window: SampleWindow::new(1_320_000, 1_500_000),
            words: (0..256).map(|i| (i as u32).wrapping_add(ep * 97)).collect(),
            pcm_duration_ms: Some(179_760),
            metrics: CaptureMetrics {
                time_mapping_version: 3,
                pcm_timeline: PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(1_320_000),
                ffmpeg_exit_code: Some(0),
                ..Default::default()
            },
        },
    };
    let target = mk_eof_outro("t3", 3);
    assert!(!complete_pcm(&target.capture));
    assert!(complete_eof_pcm(&target.capture, target.duration_ms));
    assert!(!complete_eof_pcm(&target.capture, Some(1_600_000)));
    let r1 = mk_eof_outro("r1", 1);
    let r2 = mk_eof_outro("r2", 2);
    let templates = TemplateContext {
        models: vec![TemplateModel {
            model_id: "outro-90".into(),
            version: 1,
            kind: SegmentKind::Outro,
            references: vec![
                TemplateReference {
                    sample_id: "r1".into(),
                    ledger_id: "f1".into(),
                    episode: 1,
                    source_version: "v1".into(),
                    match_interval_ms: (1_350_000, 1_440_000),
                    match_from_end_ms: Some((150_000, 60_000)),
                },
                TemplateReference {
                    sample_id: "r2".into(),
                    ledger_id: "f2".into(),
                    episode: 2,
                    source_version: "v1".into(),
                    match_interval_ms: (1_350_000, 1_440_000),
                    match_from_end_ms: Some((150_000, 60_000)),
                },
            ],
            expected_duration_ms: 90_000,
            is_stable: true,
        }],
        references: HashMap::from([("r1".into(), r1), ("r2".into(), r2)]),
    };
    let outcome = verify_template_window(
        &EmptyMatcher,
        &target,
        &templates,
        &SamplingPolicy::default(),
    );
    assert!(
        matches!(outcome, VerificationOutcome::NoMatch { .. }),
        "clean EOF tail with 240ms audio shortfall must conclude NoMatch, got {outcome:?}"
    );
}
