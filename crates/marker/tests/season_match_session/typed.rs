use super::*;
use marker::{FingerprintEngine, PairFingerprintDecision};

struct Unknown;

#[test]
fn native_silence_and_completed_absence_keep_their_typed_states_on_reversal() {
    let (_, samples) = fixture(2);
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(marker::ChromaprintEngine),
        10_000,
        32 * 1024 * 1024,
    ));
    for (words_a, words_b, expected) in [
        (
            vec![7, 7],
            vec![1, 2],
            PairFingerprintDecision::Uncertain("constant_or_silent_fingerprint".into()),
        ),
        (
            vec![1, 2],
            vec![0xfff0_0003, 0xfff0_0004],
            PairFingerprintDecision::NoMatch,
        ),
    ] {
        let mut a = samples[0].clone();
        let mut b = samples[2].clone();
        a.capture.words = words_a;
        b.capture.words = words_b;
        let (cold, work) = session
            .observe("p".into())
            .measure(|e| e.compare_evidence(&a, &b, 15., 240.));
        assert_eq!(cold, expected);
        assert_eq!(work.computed, 1);
        let (reverse, work) = session
            .observe("p".into())
            .measure(|e| e.compare_evidence(&b, &a, 15., 240.));
        assert_eq!(reverse, expected);
        assert_eq!(work.computed, 1);
        assert_eq!(work.reused, 0);
        for (first, second) in [(&a, &b), (&b, &a)] {
            let (warm, work) = session
                .observe("p".into())
                .measure(|e| e.compare_evidence(first, second, 15., 240.));
            assert_eq!(warm, expected);
            assert_eq!(work.computed, 0);
            assert_eq!(work.reused, 1);
        }
    }
}
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

#[test]
fn typed_unknown_absence_and_native_rejection_survive_hits() {
    let (_, samples) = fixture(3);
    for (engine, expected) in [
        (
            Arc::new(Unknown) as Arc<dyn FingerprintEngine>,
            FingerprintDecision::Uncertain(
                "pair_comparison_unresolved: engine_did_not_establish_absence".into(),
            ),
        ),
        (
            Arc::new(MockVerificationEngine::default()),
            FingerprintDecision::NoMatch,
        ),
    ] {
        let session = Arc::new(SeasonMatchSession::new(engine, 10_000, 32 * 1024 * 1024));
        let (cold, _) = analyze(&session, &samples);
        let (warm, work) = analyze(&session, &samples);
        assert_eq!(warm, cold);
        assert_eq!(warm[0].decision, expected);
        assert_eq!(work.computed, 0);
        assert_eq!(work.reused, 6);
    }
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(marker::ChromaprintEngine),
        10_000,
        32 * 1024 * 1024,
    ));
    let a = &samples[0];
    let b = &samples[2];
    let run = |engine: &dyn FingerprintEngine| engine.compare_evidence(a, b, 10_000., 20_000.);
    let (cold, _) = session.observe("long-policy".into()).measure(run);
    assert_eq!(
        cold,
        PairFingerprintDecision::Uncertain("native_candidates_rejected".into())
    );
    let (warm, work) = session.observe("long-policy".into()).measure(run);
    assert_eq!(warm, cold);
    assert_eq!(work.computed, 0);
    assert_eq!(work.reused, 1);
}

#[test]
fn each_argument_order_keeps_its_complete_multi_candidate_result() {
    let mut engine = MockVerificationEngine::default();
    let first = common_segment((30., 90.), (45., 105.), 0.2);
    let second = common_segment((110., 140.), (120., 150.), 0.7);
    engine
        .returns
        .insert((1, 2), vec![first.clone(), second.clone()]);
    let (_, samples) = fixture(2);
    let a = &samples[0];
    let b = &samples[2];
    // The injected engine explicitly guarantees typed/candidate projection agreement.
    assert_eq!(
        engine.compare_fingerprints(&a.capture.words, &b.capture.words, 15., 240.),
        PairFingerprintDecision::Candidates(engine.find_common_segments(
            &a.capture.words,
            &b.capture.words,
            15.,
            240.
        ))
    );
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(engine),
        10_000,
        32 * 1024 * 1024,
    ));
    let (_, cold) = session
        .observe("p".into())
        .measure(|e| e.compare_evidence(b, a, 15., 240.));
    assert_eq!(cold.computed, 1);
    let (forward, cold) = session
        .observe("p".into())
        .measure(|e| e.compare_evidence(a, b, 15., 240.));
    assert_eq!(
        forward,
        PairFingerprintDecision::Candidates(vec![first, second])
    );
    assert_eq!(cold.computed, 1);
    assert_eq!(cold.reused, 0);
    let (reverse, work) = session
        .observe("p".into())
        .measure(|e| e.compare_evidence(b, a, 15., 240.));
    assert_eq!(
        reverse,
        PairFingerprintDecision::Candidates(vec![
            common_segment((45., 105.), (30., 90.), 0.2),
            common_segment((120., 150.), (110., 140.), 0.7)
        ])
    );
    assert_eq!(work.computed, 0);
    let (_, changed_min) = session
        .observe("p".into())
        .measure(|e| e.compare_evidence(a, b, 15.000_001, 240.));
    assert_eq!(changed_min.computed, 1);
    let (_, changed_max) = session
        .observe("p".into())
        .measure(|e| e.compare_evidence(a, b, 15., 239.999_98));
    assert_eq!(changed_max.computed, 1);
}

#[test]
fn adaptive_model_template_and_boundary_share_raw_cache_but_not_final_outcomes() {
    let (engine, samples) = fixture(4);
    let session = Arc::new(SeasonMatchSession::new(
        engine.clone(),
        10_000,
        32 * 1024 * 1024,
    ));
    let policy = SamplingPolicy::default();
    let pipeline = |e: &dyn FingerprintEngine| {
        let models = marker::build_season_models(e, &samples, &policy);
        let context = marker::TemplateContext {
            models,
            references: samples
                .iter()
                .map(|e| (e.sample_id.clone(), e.clone()))
                .collect(),
        };
        samples
            .iter()
            .map(|target| marker::verify_template_window(e, target, &context, &policy))
            .collect::<Vec<_>>()
    };
    let (cold, work) = session.observe("policy-1".into()).measure(pipeline);
    assert_eq!(work.computed, 24);
    assert!(
        work.reused >= 24,
        "template and final-boundary visits are real hits"
    );
    assert!(
        cold.iter()
            .all(|d| matches!(d, marker::VerificationOutcome::Verified(_)))
    );
    let (warm, work) = session.observe("policy-1".into()).measure(pipeline);
    assert_eq!(
        serde_json::to_value(&warm).unwrap(),
        serde_json::to_value(&cold).unwrap()
    );
    assert_eq!(work.computed, 0);
    let (fresh, _) = Arc::new(SeasonMatchSession::new(engine, 0, 0))
        .observe("policy-1".into())
        .measure(pipeline);
    assert_eq!(
        serde_json::to_value(&warm).unwrap(),
        serde_json::to_value(&fresh).unwrap()
    );
}
