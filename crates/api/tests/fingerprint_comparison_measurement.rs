use api::fingerprint_job::{analysis_policy_key, measurement::EvidenceMeasurement};
use marker::{EpisodeEvidence, FingerprintEngine, PairFingerprintDecision};
use std::sync::Arc;

struct TypedEngine;
impl FingerprintEngine for TypedEngine {
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
        panic!("typed evidence must not project to legacy candidates")
    }
    fn compare_evidence(
        &self,
        a: &EpisodeEvidence,
        b: &EpisodeEvidence,
        _: f32,
        _: f32,
    ) -> PairFingerprintDecision {
        if a.source_version == b.source_version {
            PairFingerprintDecision::NoMatch
        } else {
            PairFingerprintDecision::Uncertain("injected_typed_uncertainty".into())
        }
    }
}
fn evidence(source: &str) -> EpisodeEvidence {
    EpisodeEvidence {
        sample_id: source.into(),
        ledger_id: source.into(),
        episode: 1,
        source_version: source.into(),
        capture_profile_key: "profile".into(),
        kind: marker::SegmentKind::Intro,
        capture: marker::CapturedFingerprint {
            window: marker::SampleWindow::new(0, 180_000),
            words: vec![1, 2, 3],
            pcm_duration_ms: Some(180_000),
            metrics: Default::default(),
        },
        duration_ms: Some(400_000),
    }
}

#[test]
fn observation_preserves_typed_results_and_immutable_analysis_policy() {
    let policy = marker::SamplingPolicy::default();
    let engine = Arc::new(TypedEngine);
    let a = evidence("a");
    let b = evidence("b");
    let session = Arc::new(marker::matcher::session::SeasonMatchSession::new(
        engine.clone(),
        10_000,
        32 * 1024 * 1024,
    ));
    let (results, measurement) =
        EvidenceMeasurement::new(session.clone(), &policy).measure(|wrapped| {
            [
                wrapped.compare_evidence(&a, &b, 15., 240.),
                wrapped.compare_evidence(&a, &a, 15., 240.),
            ]
        });
    assert_eq!(
        results,
        [
            PairFingerprintDecision::Uncertain("injected_typed_uncertainty".into()),
            PairFingerprintDecision::NoMatch
        ]
    );
    assert_eq!(results[0], engine.compare_evidence(&a, &b, 15., 240.));
    assert_eq!(
        measurement.analysis_policy_key,
        analysis_policy_key(&policy)
    );
    assert_eq!(measurement.computed, 2);
    assert_eq!(measurement.reused, 0);
    let (reverse, measurement) =
        EvidenceMeasurement::new(session.clone(), &policy).measure(|wrapped| {
            [
                wrapped.compare_evidence(&b, &a, 15., 240.),
                wrapped.compare_evidence(&a, &a, 15., 240.),
            ]
        });
    assert_eq!(reverse, results);
    assert_eq!(measurement.computed, 1);
    assert_eq!(measurement.reused, 1);
    let (warm, measurement) = EvidenceMeasurement::new(session, &policy).measure(|wrapped| {
        [
            wrapped.compare_evidence(&b, &a, 15., 240.),
            wrapped.compare_evidence(&a, &a, 15., 240.),
        ]
    });
    assert_eq!(warm, results);
    assert_eq!(measurement.computed, 0);
    assert_eq!(measurement.reused, 2);
    assert_eq!(measurement.compare_ns, 0);
}
