use super::{decisions::Candidate, *};
use marker::{
    EpisodeEvidence, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel,
    TemplateReference, VerificationOutcome, boundary::Interval, matcher::FingerprintDecision,
};

#[path = "head_controls.rs"]
mod controls;

fn episodes(
    captures: impl IntoIterator<Item = marker::CapturedFingerprint>,
) -> Vec<EpisodeEvidence> {
    captures
        .into_iter()
        .enumerate()
        .map(|(index, capture)| EpisodeEvidence {
            sample_id: format!("sample-{index}"),
            ledger_id: format!("file-{index}"),
            episode: index as u32 + 1,
            source_version: "measured".into(),
            capture_profile_key: "packet-clock-4".into(),
            kind: SegmentKind::Intro,
            capture,
            duration_ms: Some(30_000),
        })
        .collect()
}

fn templates(evidence: &[EpisodeEvidence], interval: Interval) -> TemplateContext {
    TemplateContext {
        models: vec![TemplateModel {
            model_id: "measured-head".into(),
            version: 1,
            kind: SegmentKind::Intro,
            references: evidence[..2]
                .iter()
                .map(|reference| TemplateReference {
                    sample_id: reference.sample_id.clone(),
                    ledger_id: reference.ledger_id.clone(),
                    episode: reference.episode,
                    source_version: reference.source_version.clone(),
                    match_interval_ms: (interval.start_ms, interval.end_ms),
                    match_from_end_ms: None,
                })
                .collect(),
            expected_duration_ms: interval.end_ms - interval.start_ms,
            is_stable: true,
        }],
        references: evidence[..2]
            .iter()
            .map(|reference| (reference.sample_id.clone(), reference.clone()))
            .collect(),
    }
}

fn assert_rejected(captured: &marker::CapturedFingerprint, start_sec: f32, label: &str) {
    let evidence = episodes(std::iter::repeat_n(captured.clone(), 3));
    let policy = SamplingPolicy::default();
    let outcomes = marker::matcher::verify_season_fingerprints_with(
        &Candidate(true, start_sec),
        &evidence,
        &Default::default(),
        &policy,
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.decision, FingerprintDecision::Uncertain(_))),
        "{label}: {outcomes:?}"
    );
    let context = templates(
        &evidence,
        marker::boundary::normalize_capture_interval(
            captured,
            Interval {
                start_ms: (start_sec * 1000.) as i64,
                end_ms: ((start_sec + 20.) * 1000.) as i64,
            },
        )
        .unwrap_or(Interval {
            start_ms: 1,
            end_ms: 19_999,
        }),
    );
    let outcome = marker::verify_template_window(
        &Candidate(true, start_sec),
        &evidence[2],
        &context,
        &policy,
    );
    assert!(
        matches!(outcome, VerificationOutcome::NeedsFullWindow { .. }),
        "{label}: {outcome:?}"
    );
}

#[test]
fn physical_head_precision_verifies_full_and_adaptive_without_fabricating_zero() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_seconds(dir.path(), 32);
    let evidence = episodes((1..=3).map(|episode| {
        let path = dir.path().join(format!("episode-{episode}.mka"));
        container::write(&path, &payloads, false, 0, |_| 0);
        capture(&path, 0, 30_000)
    }));
    for episode in &evidence {
        let source = episode.capture.metrics.source_facts.as_ref().unwrap();
        assert_eq!(source.format_start_time_us, Some(0));
        assert_eq!(source.stream_start_pts, Some(0));
        assert!(!marker::boundary::evidence::complete_pcm(&episode.capture));
        assert_eq!(
            marker::boundary::normalize_capture_interval(
                &episode.capture,
                Interval {
                    start_ms: 0,
                    end_ms: 20_000
                },
            )
            .unwrap(),
            Interval {
                start_ms: 1,
                end_ms: 19_999
            }
        );
    }
    let expected = Interval {
        start_ms: 1,
        end_ms: 19_999,
    };
    let policy = SamplingPolicy::default();
    let context = templates(&evidence, expected);
    let adaptive =
        marker::verify_template_window(&Candidate(true, 0.), &evidence[2], &context, &policy);
    match adaptive {
        VerificationOutcome::Verified(interval) => {
            assert_eq!((interval.start_ms, interval.end_ms), (1, 19_999));
            assert_eq!(interval.supporting_episodes, 2);
        }
        other => panic!("head quantization must verify conservatively: {other:?}"),
    }
    let reviewed = marker::matcher::verify_season_fingerprints_with(
        &Candidate(true, 0.),
        &evidence,
        &Default::default(),
        &policy,
    );
    assert_eq!(reviewed.len(), 3);
    for outcome in reviewed {
        assert_eq!(outcome.decision, FingerprintDecision::Verified(expected));
    }
    let absent = marker::matcher::verify_season_fingerprints_with(
        &Candidate(false, 0.),
        &evidence,
        &Default::default(),
        &policy,
    );
    assert!(
        absent
            .iter()
            .all(|outcome| matches!(outcome.decision, FingerprintDecision::Uncertain(_)))
    );
    assert!(matches!(
        marker::verify_template_window(&Candidate(false, 0.), &evidence[2], &context, &policy,),
        VerificationOutcome::NeedsFullWindow { .. }
    ));
}
