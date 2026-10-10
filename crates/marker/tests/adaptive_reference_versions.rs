use marker::fingerprint::{CaptureMetrics, CapturedFingerprint};
use marker::{CommonSegment, FingerprintEngine};
struct ScoredEngine;
impl FingerprintEngine for ScoredEngine {
    fn extract_at(&self, _: &std::path::Path, _: u32, _: u32) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        reference: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        let (start, score) = match reference[0] {
            1 => (10., 0.),
            2 => (10., 4.),
            5 => (11., 2.),
            6 => (10., 1.),
            _ => (100., 1.),
        };
        Some(CommonSegment {
            start1_sec: start,
            end1_sec: start + 60.,
            start2_sec: 10.,
            end2_sec: 70.,
            duration_sec: 60.,
            score,
        })
    }
}
fn evidence(sample_id: String, episode: u32, tag: u32) -> marker::adaptive::EpisodeEvidence {
    marker::adaptive::EpisodeEvidence {
        sample_id: sample_id.clone(),
        ledger_id: sample_id,
        episode,
        source_version: "src".into(),
        capture_profile_key: "profile".into(),
        kind: marker::SegmentKind::Intro,
        duration_ms: Some(1_000_000),
        capture: CapturedFingerprint {
            window: marker::fingerprint::SampleWindow {
                start_ms: 0,
                end_ms: 180_000,
            },
            words: vec![tag, tag + 1, tag + 2],
            pcm_duration_ms: Some(180_000),
            metrics: CaptureMetrics::default(),
        },
    }
}
fn scored_verdict(duplicates: usize) -> marker::adaptive::VerificationOutcome {
    let mut samples = vec![
        evidence("r1".into(), 1, 1),
        evidence("r2".into(), 2, 2),
        evidence("r3".into(), 3, 3),
        evidence("r4".into(), 4, 4),
    ];
    for i in 0..duplicates {
        samples.push(evidence(format!("extra-{i}"), 1, 1));
    }
    verdict_for_samples(samples)
}
fn verdict_for_samples(
    samples: Vec<marker::adaptive::EpisodeEvidence>,
) -> marker::adaptive::VerificationOutcome {
    use marker::adaptive::{SamplingPolicy, TemplateContext, TemplateModel, TemplateReference};
    let references = samples
        .iter()
        .map(|e| TemplateReference {
            sample_id: e.sample_id.clone(),
            ledger_id: e.ledger_id.clone(),
            episode: e.episode,
            source_version: e.source_version.clone(),
            match_interval_ms: (10_000, 70_000),
            match_from_end_ms: None,
        })
        .collect();
    let ctx = TemplateContext {
        models: vec![TemplateModel {
            model_id: "template".into(),
            version: 1,
            kind: marker::SegmentKind::Intro,
            references,
            expected_duration_ms: 60_000,
            is_stable: true,
        }],
        references: samples
            .into_iter()
            .map(|e| (e.sample_id.clone(), e))
            .collect(),
    };
    marker::adaptive::verify_template_window(
        &ScoredEngine,
        &evidence("target".into(), 9, 9),
        &ctx,
        &SamplingPolicy::default(),
    )
}
#[test]
fn duplicating_one_reference_must_not_reweight_cluster_scores() {
    use marker::adaptive::VerificationOutcome;
    let before = scored_verdict(0);
    let after = scored_verdict(5);
    let (VerificationOutcome::Verified(a), VerificationOutcome::Verified(b)) = (before, after)
    else {
        panic!("expected verified");
    };
    assert_eq!((a.start_ms, a.end_ms), (100_000, 160_000));
    assert_eq!(
        (b.start_ms, b.end_ms),
        (a.start_ms, a.end_ms),
        "one episode's identical copies must not change independent-consensus winner"
    );
}

#[test]
fn identical_reference_copies_must_not_change_cluster_boundaries() {
    use marker::adaptive::VerificationOutcome;
    let samples = vec![
        evidence("r1".into(), 1, 6),
        evidence("r1-other".into(), 1, 5),
        evidence("r2".into(), 2, 6),
    ];
    let before = verdict_for_samples(samples.clone());
    let mut duplicated = samples;
    for i in 0..10 {
        duplicated.push(evidence(format!("extra-{i}"), 1, 6));
    }
    let after = verdict_for_samples(duplicated);
    let (VerificationOutcome::Verified(before), VerificationOutcome::Verified(after)) =
        (before, after)
    else {
        panic!("expected verified intervals");
    };
    assert_eq!((before.start_ms, before.end_ms), (10_000, 70_000));
    assert_eq!(
        (
            after.start_ms,
            after.end_ms,
            after.score,
            after.supporting_episodes
        ),
        (
            before.start_ms,
            before.end_ms,
            before.score,
            before.supporting_episodes
        ),
        "identical copies cannot change consensus boundaries, score or independent support"
    );
}
