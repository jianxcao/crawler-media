use super::*;
use marker::{CommonSegment, FingerprintEngine, PairFingerprintDecision, SegmentKind};

pub(super) struct Candidate(pub bool, pub f32);
impl FingerprintEngine for Candidate {
    fn extract_at(
        &self,
        _: &Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(&self, _: &[u32], _: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        self.0.then_some(CommonSegment {
            start1_sec: self.1,
            end1_sec: self.1 + 20.,
            start2_sec: self.1,
            end2_sec: self.1 + 20.,
            duration_sec: 20.,
            score: 0.,
        })
    }
    fn compare_fingerprints(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> PairFingerprintDecision {
        self.find_common_segment(a, b, min, max)
            .map_or(PairFingerprintDecision::NoMatch, |segment| {
                PairFingerprintDecision::Candidates(vec![segment])
            })
    }
}

#[test]
fn ranged_capture_verification_narrows_candidates_and_cannot_claim_complete_absence() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_seconds(dir.path(), 32);
    let file = dir.path().join("proof.mka");
    container::write(&file, &payloads, true, 0, |_| 0);
    let captured = capture(&file, 0, 30_000);
    let evidence = (1..=3)
        .map(|episode| marker::EpisodeEvidence {
            sample_id: format!("sample-{episode}"),
            ledger_id: format!("file-{episode}"),
            episode,
            source_version: "measured".into(),
            capture_profile_key: "packet-clock-4".into(),
            kind: SegmentKind::Intro,
            capture: captured.clone(),
            duration_ms: Some(30_000),
        })
        .collect::<Vec<_>>();
    let policy = marker::SamplingPolicy::default();
    let reviewed = marker::matcher::verify_season_fingerprints_with(
        &Candidate(true, 5.),
        &evidence,
        &Default::default(),
        &policy,
    );
    let expected = marker::boundary::normalize_capture_interval(
        &captured,
        marker::boundary::Interval {
            start_ms: 5000,
            end_ms: 25000,
        },
    )
    .unwrap();
    assert!(expected.start_ms > 5000 && expected.end_ms < 25000);
    for outcome in reviewed {
        assert_eq!(
            outcome.decision,
            marker::matcher::FingerprintDecision::Verified(expected)
        );
    }
    let absent = marker::matcher::verify_season_fingerprints_with(
        &Candidate(false, 5.),
        &evidence,
        &Default::default(),
        &policy,
    );
    assert!(
        absent.iter().all(|outcome| matches!(
            outcome.decision,
            marker::matcher::FingerprintDecision::Uncertain(_)
        )),
        "unknown edge fractions cannot authorize full absence"
    );
}
