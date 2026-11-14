use super::*;

#[test]
fn head_exception_requires_every_current_source_and_clock_fact() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_seconds(dir.path(), 32);
    let path = dir.path().join("head.mka");
    container::write(&path, &payloads, false, 0, |_| 0);
    let captured = capture(&path, 0, 30_000);
    type Mutation = fn(&mut marker::CapturedFingerprint);
    let cases: &[(&str, Mutation)] = &[
        ("missing source", |c| c.metrics.source_facts = None),
        ("unknown format start", |c| {
            c.metrics
                .source_facts
                .as_mut()
                .unwrap()
                .format_start_time_us = None
        }),
        ("nonzero format start", |c| {
            c.metrics
                .source_facts
                .as_mut()
                .unwrap()
                .format_start_time_us = Some(1)
        }),
        ("unknown stream start", |c| {
            c.metrics.source_facts.as_mut().unwrap().stream_start_pts = None
        }),
        ("nonzero stream start", |c| {
            c.metrics.source_facts.as_mut().unwrap().stream_start_pts = Some(1)
        }),
        ("wrong stream", |c| c.metrics.audio_stream_index = Some(1)),
        ("wrong codec", |c| {
            c.metrics.source_facts.as_mut().unwrap().codec_name = Some("opus".into())
        }),
        ("unknown clock", |c| {
            c.metrics.source_facts.as_mut().unwrap().stream_time_base = None
        }),
        ("invalid", |c| {
            c.metrics.pcm_timeline = PcmTimeline::Invalid("leading loss".into())
        }),
        ("unknown", |c| c.metrics.pcm_timeline = PcmTimeline::Unknown),
        ("missing proof", |c| c.metrics.quantized_mapping = None),
        ("wide proof", |c| {
            let p = c.metrics.quantized_mapping.as_mut().unwrap();
            p.origin_min_us = -2000;
            p.origin_max_us = 2000;
        }),
        ("origin after zero", |c| {
            let p = c.metrics.quantized_mapping.as_mut().unwrap();
            p.origin_min_us = 1;
            p.origin_max_us = 999;
        }),
        ("wrong proof version", |c| {
            c.metrics.quantized_mapping.as_mut().unwrap().proof_version = 0
        }),
        ("wrong mapping version", |c| {
            c.metrics.time_mapping_version = 3
        }),
    ];
    for (label, change) in cases {
        let mut changed = captured.clone();
        change(&mut changed);
        assert_rejected(&changed, 0., label);
    }
}

#[test]
fn real_leading_loss_interior_and_tail_guards_are_never_borrowed() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_seconds(dir.path(), 32);
    let path = dir.path().join("head.mka");
    container::write(&path, &payloads, false, 0, |_| 0);
    let captured = capture(&path, 0, 30_000);
    assert_rejected(&capture(&path, 1_013, 30_000), 0., "interior source window");
    assert_rejected(&captured, 10., "tail precision cannot borrow EOF");
    let mut missing_interior = captured.clone();
    missing_interior.window.start_ms = 1;
    assert_rejected(&missing_interior, 0., "shifted requested window");
    container::write(&path, &payloads[..1000], false, 5000, |_| 0);
    let leading = capture(&path, 0, 30_000);
    assert_ne!(
        leading
            .metrics
            .source_facts
            .as_ref()
            .unwrap()
            .stream_start_pts,
        Some(0)
    );
    assert_rejected(&leading, 0., "real leading loss");
    for direction in [-1, 1] {
        container::write(&path, &payloads, false, 0, |group| {
            if group >= 12 { direction * 8 } else { 0 }
        });
        let error = marker::ChromaprintEngine
            .capture_window(&CaptureRequest {
                path: path.clone(),
                window: SampleWindow::new(0, 30_000),
                audio_stream_index: None,
                process_deadline_ms: 30_000,
            })
            .unwrap_err();
        assert!(
            error.message.contains("source_block_clock_discontinuity"),
            "{error}"
        );
        assert!(error.metrics.quantized_mapping.is_none());
    }
}

#[test]
fn head_precision_never_expands_a_candidate_or_overrides_conflicting_chapters() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads_seconds(dir.path(), 32);
    let path = dir.path().join("head.mka");
    container::write(&path, &payloads, false, 0, |_| 0);
    let captured = capture(&path, 0, 30_000);
    let policy = SamplingPolicy::default();
    assert!(
        !marker::boundary::evidence::complete_guards(
            &captured,
            Interval {
                start_ms: 0,
                end_ms: 19_999
            },
            SegmentKind::Intro,
            Some(30_000),
            &policy.boundary,
        ),
        "the physical-head guard cannot cover an unmeasured candidate coordinate"
    );
    let evidence = episodes(std::iter::repeat_n(captured, 3));
    let interval = Interval {
        start_ms: 1,
        end_ms: 19_999,
    };
    let chapters = vec![interval, interval];
    let chapter_map = evidence
        .iter()
        .map(|e| (e.ledger_id.clone(), chapters.clone()))
        .collect();
    let reviewed = marker::matcher::verify_season_fingerprints_with(
        &Candidate(true, 0.),
        &evidence,
        &chapter_map,
        &policy,
    );
    assert!(
        reviewed
            .iter()
            .all(|outcome| matches!(outcome.decision, FingerprintDecision::Uncertain(_)))
    );
    let outcome = marker::adaptive::verify_template_window_with_chapters(
        &Candidate(true, 0.),
        &evidence[2],
        &templates(&evidence, interval),
        &policy,
        &chapters,
    );
    assert!(matches!(
        outcome,
        VerificationOutcome::NeedsFullWindow { .. }
    ));
}
