use super::*;
use marker::{ChromaprintEngine, FingerprintEngine, PairFingerprintDecision};

fn block(base: u32, count: u32) -> Vec<u32> {
    (0..count)
        .map(|i| ((base + i) % 4096) << 20 | ((i * 7919 + 31) & 0xfffff))
        .collect()
}

fn samples() -> Vec<EpisodeEvidence> {
    let x = block(100, 300);
    let y = block(2200, 300);
    let mut a = block(1000, 250);
    a.extend(x.clone());
    a.extend(block(2700, 80));
    a.extend(y.clone());
    let mut b = block(1500, 250);
    b.extend(y.clone());
    b.extend(block(2800, 80));
    b.extend(x);
    let mut c = block(3000, 250);
    c.extend(y);
    c.extend(block(3300, 300));
    [("z", a), ("a", b), ("c", c)]
        .into_iter()
        .enumerate()
        .map(|(index, (file, words))| EpisodeEvidence {
            sample_id: file.into(),
            ledger_id: file.into(),
            episode: index as u32 + 1,
            source_version: "v1".into(),
            capture_profile_key: "p1".into(),
            kind: SegmentKind::Intro,
            capture: marker::CapturedFingerprint {
                window: marker::SampleWindow::new(0, 180_000),
                words,
                pcm_duration_ms: Some(180_000),
                metrics: marker::CaptureMetrics {
                    pcm_timeline: marker::PcmTimeline::Contiguous,
                    contiguous_pcm_start_ms: Some(0),
                    ..Default::default()
                },
            },
            duration_ms: Some(180_000),
        })
        .collect()
}

#[test]
fn native_directional_candidates_match_direct_engine_in_disabled_cold_and_warm_sessions() {
    let samples = samples();
    let direct = |a, b| ChromaprintEngine.compare_evidence(a, b, 15., 240.);
    let forward = direct(&samples[0], &samples[1]);
    let reverse = direct(&samples[1], &samples[0]);
    assert_eq!(
        forward,
        PairFingerprintDecision::Candidates(vec![marker::CommonSegment {
            start1_sec: 77.876_19,
            end1_sec: 115.142_85,
            start2_sec: 30.828_571,
            end2_sec: 68.095_24,
            duration_sec: 37.266_666,
            score: 0.059800664451827246,
        }])
    );
    // Native tie selection chooses X in the reverse direction, rather than Y.
    assert_eq!(
        reverse,
        PairFingerprintDecision::Candidates(vec![marker::CommonSegment {
            start1_sec: 77.876_19,
            end1_sec: 115.142_85,
            start2_sec: 30.828_571,
            end2_sec: 68.095_24,
            duration_sec: 37.266_666,
            score: 0.07641196013289037,
        }])
    );
    assert_ne!(forward, reverse);
    for capacity in [0, 10_000] {
        let session = Arc::new(SeasonMatchSession::new(
            Arc::new(ChromaprintEngine),
            capacity,
            32 * 1024 * 1024,
        ));
        for pass in 0..2 {
            for (a, b, expected) in [
                (&samples[0], &samples[1], &forward),
                (&samples[1], &samples[0], &reverse),
            ] {
                let (actual, work) = session
                    .observe("native".into())
                    .measure(|e| e.compare_evidence(a, b, 15., 240.));
                assert_eq!(&actual, expected, "capacity={capacity}, pass={pass}");
                let miss = capacity == 0 || pass == 0;
                assert_eq!(work.computed, u64::from(miss));
                assert_eq!(work.reused, u64::from(!miss));
            }
        }
    }
}

#[test]
fn native_file_decisions_match_direct_engine_for_each_input_order() {
    let original = samples();
    let direct = |samples: &[EpisodeEvidence]| {
        marker::matcher::verify_season_fingerprints_with(
            &ChromaprintEngine,
            samples,
            &HashMap::new(),
            &SamplingPolicy::default(),
        )
    };
    assert_eq!(
        direct(&original)[0].decision,
        FingerprintDecision::Verified(marker::boundary::Interval {
            start_ms: 77_876,
            end_ms: 115_143,
        })
    );
    for capacity in [0, 10_000] {
        let session = Arc::new(SeasonMatchSession::new(
            Arc::new(ChromaprintEngine),
            capacity,
            32 * 1024 * 1024,
        ));
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2]] {
            let reordered = order.map(|i| original[i].clone());
            let expected = direct(&reordered);
            for pass in 0..2 {
                let (actual, work) = analyze(&session, &reordered);
                assert_eq!(actual, expected, "capacity={capacity}, order={order:?}");
                if capacity == 0 {
                    assert!(work.computed > 0);
                    assert_eq!(work.reused, 0);
                } else if pass == 1 {
                    assert_eq!(work.computed, 0);
                    assert_eq!(work.compare_ns, 0);
                    assert!(work.reused > 0);
                }
            }
        }
    }
}
