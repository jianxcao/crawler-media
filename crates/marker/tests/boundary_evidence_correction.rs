use marker::boundary::evidence::complete_guards;
use marker::boundary::{
    BoundaryDecision, BoundaryEvidence, BoundaryPolicy, Interval, refine_boundary,
};
use marker::{CaptureMetrics, CapturedFingerprint, SampleWindow, SegmentKind};

fn capture(start: i64, end: i64, pcm: i64) -> CapturedFingerprint {
    CapturedFingerprint {
        window: SampleWindow::new(start, end),
        words: vec![1, 2, 3, 4],
        pcm_duration_ms: Some(pcm),
        metrics: CaptureMetrics::default(),
    }
}

fn guarded(capture: &CapturedFingerprint, start: i64, end: i64, duration: Option<i64>) -> bool {
    complete_guards(
        capture,
        Interval {
            start_ms: start,
            end_ms: end,
        },
        SegmentKind::Outro,
        duration,
        &BoundaryPolicy::default(),
    )
}

#[test]
fn guard_context_intersects_proven_media_bounds() {
    assert!(guarded(
        &capture(0, 180_000, 180_000),
        123,
        107_590,
        Some(1_000_000)
    ));
    assert!(guarded(
        &capture(820_000, 1_000_000, 179_999),
        830_000,
        997_295,
        Some(1_000_000)
    ));
    assert!(guarded(
        &capture(0, 60_000, 59_980),
        123,
        57_295,
        Some(60_000)
    ));
}

#[test]
fn sample_edges_cannot_replace_missing_real_media_context() {
    assert!(!guarded(
        &capture(10_000, 190_000, 180_000),
        10_123,
        107_590,
        Some(1_000_000)
    ));
    assert!(!guarded(
        &capture(820_000, 1_000_000, 180_000),
        830_000,
        997_295,
        None
    ));
    assert!(!guarded(
        &capture(0, 180_000, 180_000),
        30_000,
        177_295,
        Some(1_000_000)
    ));
}

#[test]
fn invalid_or_overflowing_guard_metadata_abstains() {
    for (start, end, duration) in [
        (-1, 30_000, Some(60_000)),
        (40_000, 30_000, Some(60_000)),
        (0, 60_001, Some(60_000)),
        (0, 30_000, Some(0)),
    ] {
        assert!(!guarded(&capture(0, 60_000, 60_000), start, end, duration));
    }
    assert!(!guarded(
        &capture(i64::MAX - 60_000, i64::MAX, 60_000),
        i64::MAX - 30_000,
        i64::MAX - 1,
        Some(i64::MAX)
    ));
    assert!(!guarded(
        &capture(0, 60_000, -1),
        10_000,
        30_000,
        Some(60_000)
    ));
}

fn observed(capture: &CapturedFingerprint, chapters: &[Interval]) -> BoundaryEvidence {
    marker::boundary::evidence::collect_boundary_evidence(
        capture,
        Interval {
            start_ms: 30_000,
            end_ms: 90_000,
        },
        chapters,
        SegmentKind::Intro,
        Some(120_000),
        marker::boundary::evidence::ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    )
}

#[test]
fn raw_rms_chatter_cannot_veto_supported_candidate() {
    let mut sample = capture(0, 120_000, 120_000);
    sample.metrics.pcm_frame_ms = 20;
    sample.metrics.pcm_rms_dbfs_milli = vec![-20_000; 6000];
    let baseline = refine_boundary(&observed(&sample, &[]), &BoundaryPolicy::default());
    for range in [1400..1500, 4500..4600, 4700..4800] {
        sample.metrics.pcm_rms_dbfs_milli[range].fill(-100_000);
        let evidence = observed(&sample, &[]);
        assert!(
            !evidence.conflicting_evidence,
            "RMS observations alone are not semantic conflict"
        );
        assert_eq!(
            refine_boundary(&evidence, &BoundaryPolicy::default()),
            baseline
        );
    }
}

#[test]
fn no_chapter_audio_proposal_cannot_change_eligibility_or_bounds() {
    let mut evidence = observed(&capture(0, 120_000, 120_000), &[]);
    for proposal in [
        Interval {
            start_ms: 30_500,
            end_ms: 89_500,
        },
        Interval {
            start_ms: 20_000,
            end_ms: 100_000,
        },
    ] {
        evidence.audio_transition = Some(proposal);
        assert_eq!(
            refine_boundary(&evidence, &BoundaryPolicy::default()),
            BoundaryDecision::Verified(Interval {
                start_ms: 30_000,
                end_ms: 90_000
            })
        );
    }
    evidence.complete_guards = false;
    assert!(matches!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
    evidence.complete_guards = true;
    evidence.references_agree = false;
    assert!(matches!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
}

#[test]
fn chapters_still_require_corroboration_and_preserve_supported_conflicts() {
    let sample = capture(0, 120_000, 120_000);
    for chapters in [
        vec![Interval {
            start_ms: 30_000,
            end_ms: 90_000,
        }],
        vec![Interval {
            start_ms: 0,
            end_ms: 120_000,
        }],
        vec![
            Interval {
                start_ms: 30_000,
                end_ms: 90_000,
            },
            Interval {
                start_ms: 31_000,
                end_ms: 89_000,
            },
        ],
    ] {
        assert!(matches!(
            refine_boundary(&observed(&sample, &chapters), &BoundaryPolicy::default()),
            BoundaryDecision::Uncertain(_)
        ));
    }
    let mut evidence = observed(
        &sample,
        &[Interval {
            start_ms: 30_500,
            end_ms: 89_900,
        }],
    );
    evidence.audio_transition = Some(Interval {
        start_ms: 30_800,
        end_ms: 90_000,
    });
    assert_eq!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Verified(Interval {
            start_ms: 30_800,
            end_ms: 89_900
        })
    );
    evidence.audio_transition = Some(Interval {
        start_ms: 32_000,
        end_ms: 90_000,
    });
    assert!(matches!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
    evidence.audio_transition = Some(Interval {
        start_ms: 30_800,
        end_ms: 90_000,
    });
    evidence.conflicting_evidence = true;
    assert!(matches!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Uncertain(_)
    ));
}
