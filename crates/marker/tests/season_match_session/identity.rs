use super::*;

#[test]
fn every_raw_identity_change_invalidates_only_its_own_pairs() {
    let (engine, samples) = fixture(4);
    let mutations: &[fn(&mut EpisodeEvidence)] = &[
        |e| e.ledger_id.push_str("-other-cut"),
        |e| e.source_version.push_str("-v2"),
        |e| e.capture_profile_key.push_str("-audio-2"),
        |e| e.capture.window.start_ms += 1,
        |e| e.capture.window.end_ms -= 1,
        |e| e.capture.words[1] ^= 1,
        |e| e.capture.words.push(999),
        |e| e.capture.metrics.audio_stream_index = Some(2),
        |e| e.capture.metrics.time_mapping_version += 1,
        |e| e.kind = SegmentKind::Outro,
    ];
    for mutate in mutations {
        let session = Arc::new(SeasonMatchSession::new(
            engine.clone(),
            10_000,
            32 * 1024 * 1024,
        ));
        analyze(&session, &samples);
        let mut changed = samples.clone();
        mutate(&mut changed[0]);
        let (decisions, work) = analyze(&session, &changed);
        assert_eq!(
            decisions,
            analyze(
                &Arc::new(SeasonMatchSession::new(engine.clone(), 0, 0)),
                &changed
            )
            .0
        );
        assert_eq!(work.computed, 6);
        assert!(work.reused >= 9);
    }
    let session = Arc::new(SeasonMatchSession::new(engine, 10_000, 32 * 1024 * 1024));
    analyze(&session, &samples);
    let (_, changed_policy) = session.observe("policy-2".into()).measure(|e| {
        marker::matcher::verify_season_fingerprints_with(
            e,
            &samples,
            &HashMap::new(),
            &SamplingPolicy::default(),
        )
    });
    assert_eq!(changed_policy.computed, 24);
}

#[test]
fn input_order_and_reference_removal_recompute_current_consensus() {
    let (engine, samples) = fixture(4);
    let session = Arc::new(SeasonMatchSession::new(
        engine.clone(),
        10_000,
        32 * 1024 * 1024,
    ));
    let (original, _) = analyze(&session, &samples);
    let mut reversed = samples.clone();
    reversed.reverse();
    let (mut reordered, work) = analyze(&session, &reversed);
    reordered.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
    let mut expected = original;
    expected.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
    assert_eq!(reordered, expected);
    assert_eq!(work.computed, 0);
    let (removed, work) = analyze(&session, &samples[..4]);
    assert_eq!(
        removed,
        analyze(
            &Arc::new(SeasonMatchSession::new(engine, 0, 0)),
            &samples[..4]
        )
        .0
    );
    assert!(
        removed
            .iter()
            .all(|d| matches!(d.decision, FingerprintDecision::Uncertain(_)))
    );
    assert_eq!(work.computed, 0);
}

#[test]
fn two_files_of_one_episode_keep_separate_intervals_and_one_support_vote() {
    let (engine, mut samples) = fixture(4);
    samples.retain(|e| e.kind == SegmentKind::Intro);
    samples[3].episode = 1;
    samples[3].ledger_id = "another-file-for-episode-1".into();
    samples[3].capture.window = marker::SampleWindow::new(5_000, 185_000);
    let session = Arc::new(SeasonMatchSession::new(engine, 10_000, 32 * 1024 * 1024));
    let (decisions, _) = analyze(&session, &samples);
    assert_eq!(
        decisions[0].decision,
        FingerprintDecision::Verified(marker::boundary::Interval {
            start_ms: 30_000,
            end_ms: 90_000
        })
    );
    assert_eq!(
        decisions[3].decision,
        FingerprintDecision::Verified(marker::boundary::Interval {
            start_ms: 35_000,
            end_ms: 95_000
        })
    );
    assert_ne!(decisions[0].ledger_id, decisions[3].ledger_id);
    let references = vec![samples[0].clone(), samples[1].clone(), samples[3].clone()];
    let (removed, _) = analyze(&session, &references);
    assert!(
        removed
            .iter()
            .all(|d| matches!(d.decision, FingerprintDecision::Uncertain(_)))
    );
}

#[test]
fn pcm_and_chapters_are_live_even_when_all_raw_evidence_is_reused() {
    let (engine, mut samples) = fixture(3);
    let session = Arc::new(SeasonMatchSession::new(
        engine.clone(),
        10_000,
        32 * 1024 * 1024,
    ));
    analyze(&session, &samples);
    // Short, known-aligned PCM retains raw eligibility but loses right guard.
    samples[0].capture.metrics.contiguous_pcm_start_ms = Some(0);
    samples[0].capture.pcm_duration_ms = Some(91_000);
    let (changed, work) = analyze(&session, &samples);
    assert_eq!(work.computed, 0);
    assert!(work.reused > 0);
    assert!(matches!(
        changed[0].decision,
        FingerprintDecision::Uncertain(_)
    ));
    assert_eq!(
        changed,
        analyze(
            &Arc::new(SeasonMatchSession::new(engine.clone(), 0, 0)),
            &samples
        )
        .0
    );
    samples[0].capture.pcm_duration_ms = Some(180_000);
    let chapters = HashMap::from([(
        samples[0].ledger_id.clone(),
        vec![marker::boundary::Interval {
            start_ms: 31_000,
            end_ms: 89_000,
        }],
    )]);
    let run = |e: &dyn marker::FingerprintEngine| {
        marker::matcher::verify_season_fingerprints_with(
            e,
            &samples,
            &chapters,
            &SamplingPolicy::default(),
        )
    };
    let (changed, work) = session.observe("policy-1".into()).measure(run);
    assert_eq!(work.computed, 0);
    assert!(matches!(
        changed[0].decision,
        FingerprintDecision::Uncertain(_)
    ));
    assert_eq!(
        changed,
        Arc::new(SeasonMatchSession::new(engine, 0, 0))
            .observe("policy-1".into())
            .measure(run)
            .0
    );
}
