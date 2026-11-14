use super::*;

#[test]
fn model_truncation_and_verifier_rounding_keep_their_existing_millisecond_boundaries() {
    let (engine, mut samples) = fixture(3);
    let mut engine = (*engine).clone();
    for candidates in engine.returns.values_mut() {
        *candidates = vec![common_segment(
            (30.000_6, 90.000_6),
            (30.000_6, 90.000_6),
            0.5,
        )];
    }
    // Nonzero, unequal windows exercise normalization rather than a duration offset.
    samples.retain(|e| e.kind == SegmentKind::Outro);
    let session = Arc::new(SeasonMatchSession::new(
        Arc::new(engine),
        10_000,
        32 * 1024 * 1024,
    ));
    let (models, _) = session
        .observe("p".into())
        .measure(|e| marker::build_season_models(e, &samples, &SamplingPolicy::default()));
    assert_eq!(models.len(), 1);
    let reference = models[0]
        .references
        .iter()
        .find(|r| r.episode == 1)
        .unwrap();
    assert_eq!(reference.match_interval_ms, (931_000, 991_000));
    let (decisions, work) = analyze(&session, &samples);
    assert_eq!(
        decisions[0].decision,
        FingerprintDecision::Verified(marker::boundary::Interval {
            start_ms: 931_001,
            end_ms: 991_001
        })
    );
    // The distinct policy identity above deliberately requires separate entries.
    assert_eq!(work.computed, 6);
}

#[test]
fn adaptive_models_reject_candidates_outside_actual_windows() {
    let (engine, mut samples) = fixture(3);
    samples.retain(|e| e.kind == SegmentKind::Intro);
    for e in &mut samples {
        e.capture.window.end_ms = 60_000;
        e.capture.pcm_duration_ms = Some(60_000);
    }
    let models = marker::build_season_models(engine.as_ref(), &samples, &SamplingPolicy::default());
    assert!(
        models.is_empty(),
        "a 30-90 second candidate is outside each actual 0-60 second capture"
    );
}
