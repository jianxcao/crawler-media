mod common;

use std::{collections::HashMap, sync::Arc};

use common::{MockVerificationEngine, common_segment, make_target_evidence};
use marker::matcher::session::SeasonMatchSession;
use marker::matcher::{EpisodeFingerprintDecision, FingerprintDecision};
use marker::{EpisodeEvidence, SamplingPolicy, SegmentKind};

#[path = "season_match_session/coordinates.rs"]
mod coordinates;
#[path = "season_match_session/identity.rs"]
mod identity;
#[path = "season_match_session/native_orientation.rs"]
mod native_orientation;
#[path = "season_match_session/resources.rs"]
mod resources;
#[path = "season_match_session/typed.rs"]
mod typed;

fn fixture(count: u32) -> (Arc<MockVerificationEngine>, Vec<EpisodeEvidence>) {
    let mut engine = MockVerificationEngine::default();
    for a in 1..=count {
        for b in a + 1..=count {
            engine
                .returns
                .insert((a, b), vec![common_segment((30., 90.), (30., 90.), 0.5)]);
        }
    }
    let samples = (1..=count)
        .flat_map(|episode| {
            [SegmentKind::Intro, SegmentKind::Outro].map(move |kind| {
                let start = if kind == SegmentKind::Intro {
                    0
                } else {
                    900_000 + i64::from(episode) * 1_000
                };
                make_target_evidence(
                    &format!("{episode}-{kind:?}"),
                    episode,
                    kind,
                    episode,
                    start,
                    start + 180_000,
                )
            })
        })
        .collect();
    (Arc::new(engine), samples)
}

fn analyze(
    session: &Arc<SeasonMatchSession>,
    samples: &[EpisodeEvidence],
) -> (
    Vec<EpisodeFingerprintDecision>,
    marker::matcher::session::PairWork,
) {
    session.observe("policy-1".into()).measure(|engine| {
        marker::matcher::verify_season_fingerprints_with(
            engine,
            samples,
            &HashMap::new(),
            &SamplingPolicy::default(),
        )
    })
}

#[test]
fn incremental_analysis_reuses_raw_pairs_and_rechecks_each_file_boundary() {
    let (engine, samples) = fixture(4);
    let session = Arc::new(SeasonMatchSession::new(
        engine.clone(),
        10_000,
        32 * 1024 * 1024,
    ));
    let (_, initial) = analyze(&session, &samples[..6]);
    assert_eq!(initial.computed, 12);
    assert!(
        initial.reused >= 6,
        "boundary reviews must use the same cache"
    );
    let (incremental, work) = analyze(&session, &samples);
    assert_eq!(
        incremental,
        marker::matcher::verify_season_fingerprints_with(
            engine.as_ref(),
            &samples,
            &HashMap::new(),
            &SamplingPolicy::default()
        )
    );
    let fresh = Arc::new(SeasonMatchSession::new(engine, 0, 0));
    assert_eq!(incremental, analyze(&fresh, &samples).0);
    assert_eq!(work.computed, 12);
    assert!(work.reused >= 24);
    assert_eq!(incremental.len(), 8);
    for decision in incremental {
        let start = if decision.kind == SegmentKind::Intro {
            0
        } else {
            900_000 + i64::from(decision.episode) * 1_000
        };
        assert_eq!(decision.ledger_id, format!("ledger-{}", decision.episode));
        assert_eq!(
            decision.decision,
            FingerprintDecision::Verified(marker::boundary::Interval {
                start_ms: start + 30_000,
                end_ms: start + 90_000
            })
        );
    }
    let (_, warm) = analyze(&session, &samples);
    assert_eq!(warm.computed, 0);
    assert_eq!(warm.compare_ns, 0);
    assert!(warm.reused >= 36);
}
