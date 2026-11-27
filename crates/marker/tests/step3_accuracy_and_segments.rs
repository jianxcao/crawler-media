//! Step 3 TDD regression suite for Chromaprint diagonal gap bridging across brief
//! dialogue/SFX spikes, short (<6s) studio bumper NoMatch classification, and
//! early Outro adaptive window prediction before long post-credit scenes.

mod common;

use common::{intro_template_model, make_target_evidence, template_reference};
use marker::adaptive::{
    EpisodeDescriptor, SamplingPolicy, SegmentKind, SourceCostSummary, TemplateContext,
    TemplateModel, TemplateReference, VerificationOutcome, WindowDecision, plan_episode_window,
    verify_template_window,
};
use marker::{ChromaprintEngine, FingerprintEngine, PairFingerprintDecision};

fn distinct_words(seed: u32, len: usize) -> Vec<u32> {
    (0..len as u32)
        .map(|i| {
            let mut x = i
                .wrapping_add(1)
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(seed.wrapping_mul(0x85EB_CA6B));
            x ^= x >> 16;
            x = x.wrapping_mul(0x7FEB_352D);
            x ^= x >> 15;
            x = x.wrapping_mul(0x846C_A68B);
            x ^= x >> 16;
            x
        })
        .collect()
}

#[test]
fn brief_dialogue_spike_inside_op_bridges_into_full_intro_and_avoids_self_conflict() {
    let total_items = 1454usize; // ~180s
    let op_start = 160usize; // ~19.8s
    let op_len = 726usize; // ~89.9s -> ends at ~109.7s
    let shared_op = distinct_words(9001, op_len);

    let mut ep1 = distinct_words(101, total_items);
    let mut ep2 = distinct_words(202, total_items);
    let mut ep3 = distinct_words(303, total_items);

    for (idx, &word) in shared_op.iter().enumerate() {
        ep1[op_start + idx] = word;
        ep2[op_start + idx] = word ^ 0b01; // slight benign compression noise (score 1)
        ep3[op_start + idx] = word ^ 0b10; // slight benign compression noise (score 1)
    }

    // Episode 1 has a 1.7s (14 items) narrator/dialogue voiceover spike right in the middle of the OP!
    let spike_offset = op_start + 355;
    for item in &mut ep1[spike_offset..spike_offset + 14] {
        *item ^= 0x0FFF_0FFF; // Hamming distance ~24 > max_score (4.0)
    }

    let seg = marker::find_common_segment(&ep1, &ep2, 15.0, 240.0)
        .expect("90s OP with 1.7s voiceover spike must still yield a candidate");
    assert!(
        (87.0..93.0).contains(&seg.duration_sec),
        "expected full ~90s OP to bridge 1.7s voiceover gap, got {seg:?}"
    );

    let consensus = marker::match_episodes_fingerprints(
        &[(1, ep1.clone()), (2, ep2.clone()), (3, ep3.clone())],
        15.0,
        240.0,
    );
    assert_eq!(
        consensus.len(),
        3,
        "all 3 episodes must reach consensus without splitting into conflicting halves: {consensus:?}"
    );
    for m in &consensus {
        let dur_ms = m.intro_end_ms - m.intro_start_ms;
        assert!(
            (87_000..=93_000).contains(&dur_ms),
            "episode {} published truncated intro {}..{} ({dur_ms}ms)",
            m.episode,
            m.intro_start_ms,
            m.intro_end_ms
        );
    }

    // Safety guard: a sustained 8-second (65 items) divergent scene in the middle MUST NOT bridge.
    let mut ep_long_break = ep2.clone();
    for item in &mut ep_long_break[op_start + 330..op_start + 395] {
        *item ^= 0x0FFF_0FFF;
    }
    if let Some(unbridged) = marker::find_common_segment(&ep_long_break, &ep3, 15.0, 240.0) {
        assert!(
            unbridged.duration_sec < 50.0,
            "sustained 8s non-matching audio must not be bridged into a single 90s segment: {unbridged:?}"
        );
    }
}

#[test]
fn short_studio_bumper_under_six_seconds_reports_no_match_while_longer_rejected_candidate_stays_uncertain()
 {
    let total_items = 1454usize;
    let shared_bumper = distinct_words(777, 32); // ~3.96s (< 6.0s studio logo)

    let mut a = distinct_words(11, total_items);
    let mut b = distinct_words(22, total_items);
    a[20..52].copy_from_slice(&shared_bumper);
    b[20..52].copy_from_slice(&shared_bumper);

    let decision = ChromaprintEngine.compare_fingerprints(&a, &b, 15.0, 240.0);
    assert_eq!(
        decision,
        PairFingerprintDecision::NoMatch,
        "a <6s studio logo sting without any OP must report NoMatch, got {decision:?}"
    );

    // Verify adaptive verification on a complete 180s window also resolves to NoMatch (not NeedsFullWindow).
    let mut ref1 = make_target_evidence("ref-1", 1, SegmentKind::Intro, 1, 0, 180_000);
    let mut ref2 = make_target_evidence("ref-2", 2, SegmentKind::Intro, 2, 0, 180_000);
    let mut target = make_target_evidence("target", 3, SegmentKind::Intro, 3, 0, 180_000);
    let shared_op = distinct_words(555, 720);
    let mut r1_words = distinct_words(1001, total_items);
    let mut r2_words = distinct_words(1002, total_items);
    let mut t_words = distinct_words(1003, total_items);
    r1_words[20..52].copy_from_slice(&shared_bumper);
    r2_words[20..52].copy_from_slice(&shared_bumper);
    t_words[20..52].copy_from_slice(&shared_bumper);
    r1_words[100..820].copy_from_slice(&shared_op);
    r2_words[100..820].copy_from_slice(&shared_op);
    ref1.capture.words = r1_words;
    ref2.capture.words = r2_words;
    target.capture.words = t_words;

    let mut templates = TemplateContext::default();
    templates.models.push(intro_template_model(
        "intro-90",
        vec![
            template_reference("ref-1", 1, (12_400, 101_500)),
            template_reference("ref-2", 2, (12_400, 101_500)),
        ],
        89_100,
    ));
    templates.references.insert("ref-1".into(), ref1);
    templates.references.insert("ref-2".into(), ref2);

    let outcome = verify_template_window(
        &ChromaprintEngine,
        &target,
        &templates,
        &SamplingPolicy::default(),
    );
    assert!(
        matches!(outcome, VerificationOutcome::NoMatch { .. }),
        "complete window with only a 4s studio logo must conclude NoMatch, got {outcome:?}"
    );

    // Contract preservation: a 10s (82 items) rejected candidate (>= 6s, < 15s min) MUST remain Uncertain!
    let shared_10s = distinct_words(888, 82);
    let mut c = distinct_words(33, total_items);
    let mut d = distinct_words(44, total_items);
    c[100..182].copy_from_slice(&shared_10s);
    d[100..182].copy_from_slice(&shared_10s);
    assert_eq!(
        ChromaprintEngine.compare_fingerprints(&c, &d, 15.0, 240.0),
        PairFingerprintDecision::Uncertain("native_candidates_rejected".into()),
        "borderline >=6s candidate below min_duration must stay Uncertain"
    );
}

#[test]
fn early_outro_prediction_preserves_bounded_tail_window_beyond_180s() {
    let policy = SamplingPolicy::default();
    let template = TemplateModel {
        model_id: "outro-early".into(),
        version: 1,
        kind: SegmentKind::Outro,
        references: (1..=3)
            .map(|ep| TemplateReference {
                sample_id: format!("s{ep}"),
                ledger_id: format!("l{ep}"),
                episode: ep,
                source_version: "v1".into(),
                match_interval_ms: (1_250_000, 1_340_000),
                // 90s ED followed by 160s post-credit epilogue + preview!
                match_from_end_ms: Some((250_000, 160_000)),
            })
            .collect(),
        expected_duration_ms: 90_000,
        is_stable: true,
    };
    let templates = TemplateContext {
        models: vec![template],
        ..Default::default()
    };
    let target = EpisodeDescriptor {
        ledger_id: "l4".into(),
        episode: 4,
        source_version: "v1".into(),
        duration_ms: 1_500_000,
    };
    let decision = plan_episode_window(
        &target,
        SegmentKind::Outro,
        &templates,
        &policy,
        &SourceCostSummary::default(),
    );
    match decision {
        WindowDecision::Verify { window, .. } => {
            assert_eq!(
                (window.start_ms, window.end_ms),
                (1_240_000, 1_350_000),
                "early outro within 300s tail bound must not be clamped to 180s default tail (1_320_000)"
            );
        }
        other => panic!("expected Verify window [1_240_000, 1_350_000], got {other:?}"),
    }
}
