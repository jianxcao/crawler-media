mod common;

use common::{
    MockVerificationEngine, common_segment, intro_template_model, make_target_evidence,
    template_reference,
};
use marker::{
    CommonSegment, SamplingPolicy, SegmentKind, TemplateContext, VerificationOutcome,
    match_episodes_fingerprints_with, match_episodes_outros_with, verify_template_window,
};

fn episodes(count: u32) -> Vec<(u32, Vec<u32>)> {
    (1..=count).map(|n| (n, vec![n, n + 10])).collect()
}

fn repeated(candidates: Vec<CommonSegment>, count: u32) -> MockVerificationEngine {
    let mut engine = MockVerificationEngine::default();
    for first in 1..=count {
        for second in first + 1..=count {
            engine.returns.insert((first, second), candidates.clone());
        }
    }
    engine
}

#[test]
fn repeated_low_quality_or_nonfinite_audio_is_not_published() {
    for score in [9.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let engine = repeated(vec![common_segment((0.0, 70.0), (0.0, 70.0), score)], 4);
        assert!(
            match_episodes_fingerprints_with(&engine, &episodes(4), 15.0, 240.0).is_empty(),
            "score {score}"
        );
    }
}

#[test]
fn quality_precedes_duration_for_intro_and_outro() {
    let engine = repeated(
        vec![
            common_segment((0.0, 70.0), (0.0, 70.0), 3.9),
            common_segment((90.0, 150.0), (90.0, 150.0), 0.5),
        ],
        4,
    );
    let detected = match_episodes_fingerprints_with(&engine, &episodes(4), 15.0, 240.0);
    assert_eq!(detected.len(), 4);
    assert!(
        detected
            .iter()
            .all(|v| v.intro_start_ms == 90_000 && v.intro_end_ms == 150_000)
    );
    let outros = episodes(4)
        .into_iter()
        .map(|(n, words)| (n, words, 600_000))
        .collect::<Vec<_>>();
    let detected = match_episodes_outros_with(&engine, &outros, 15.0, 240.0);
    assert_eq!(detected.len(), 4);
    assert!(
        detected
            .iter()
            .all(|v| v.outro_start_ms == 690_000 && v.outro_end_ms == 750_000)
    );
}

#[test]
fn equal_evidence_conflicting_intervals_remain_unpublished_even_if_one_is_longer() {
    let engine = repeated(
        vec![
            common_segment((0.0, 70.0), (0.0, 70.0), 0.5),
            common_segment((90.0, 150.0), (90.0, 150.0), 0.5),
        ],
        4,
    );
    assert!(match_episodes_fingerprints_with(&engine, &episodes(4), 15.0, 240.0).is_empty());
}

#[test]
fn overlapping_equal_evidence_must_not_create_an_unsupported_median_interval() {
    let engine = repeated(
        vec![
            common_segment((0.0, 70.0), (0.0, 70.0), 0.5),
            common_segment((10.0, 80.0), (10.0, 80.0), 0.5),
        ],
        4,
    );
    assert!(match_episodes_fingerprints_with(&engine, &episodes(4), 15.0, 240.0).is_empty());
}

#[test]
fn equivalent_boundary_evidence_uses_duration_only_as_the_last_tiebreaker() {
    let engine = repeated(
        vec![
            common_segment((0.0, 70.0), (0.0, 70.0), 0.5),
            common_segment((0.0, 70.5), (0.0, 70.5), 0.5),
        ],
        4,
    );
    let result = match_episodes_fingerprints_with(&engine, &episodes(4), 15.0, 240.0);
    assert_eq!(result.len(), 4);
    assert!(result.iter().all(|v| v.intro_end_ms == 70_500));
}

fn shared_reference_occurrences(reverse: bool, later_score: f64) -> MockVerificationEngine {
    let mut engine = MockVerificationEngine::default();
    for reference in [2, 3] {
        let mut candidates = vec![
            common_segment((0.0, 70.0), (0.0, 70.0), 0.5),
            common_segment((90.0, 160.0), (0.0, 70.0), later_score),
        ];
        if reverse {
            candidates.reverse();
        }
        engine.returns.insert((1, reference), candidates);
    }
    engine
        .returns
        .insert((2, 3), vec![common_segment((0.0, 70.0), (0.0, 70.0), 0.5)]);
    engine
}

const EPISODE_ORDERS: [[u32; 3]; 6] = [
    [1, 2, 3],
    [1, 3, 2],
    [2, 1, 3],
    [2, 3, 1],
    [3, 1, 2],
    [3, 2, 1],
];

#[test]
fn intro_shared_reference_occurrences_abstain_only_for_the_ambiguous_target() {
    for order in EPISODE_ORDERS {
        for reverse in [false, true] {
            let episodes = order.map(|n| (n, vec![n, n + 10]));
            let mut result = match_episodes_fingerprints_with(
                &shared_reference_occurrences(reverse, 0.5),
                &episodes,
                15.0,
                240.0,
            );
            result.sort_by_key(|v| v.episode);
            assert_eq!(
                result
                    .iter()
                    .map(|v| (v.episode, v.intro_start_ms, v.intro_end_ms))
                    .collect::<Vec<_>>(),
                [(2, 0, 70_000), (3, 0, 70_000)],
                "order={order:?}, reverse={reverse}"
            );
        }
    }
}

#[test]
fn outro_shared_reference_occurrences_abstain_only_for_the_ambiguous_target() {
    for order in EPISODE_ORDERS {
        for reverse in [false, true] {
            let episodes = order.map(|n| (n, vec![n, n + 10], i64::from(n) * 600_000));
            let mut result = match_episodes_outros_with(
                &shared_reference_occurrences(reverse, 0.5),
                &episodes,
                15.0,
                240.0,
            );
            result.sort_by_key(|v| v.episode);
            assert_eq!(
                result
                    .iter()
                    .map(|v| (v.episode, v.outro_start_ms, v.outro_end_ms))
                    .collect::<Vec<_>>(),
                [(2, 1_200_000, 1_270_000), (3, 1_800_000, 1_870_000)],
                "order={order:?}, reverse={reverse}"
            );
        }
    }
}

#[test]
fn shared_reference_occurrences_still_select_the_stronger_target_hypothesis() {
    for reverse in [false, true] {
        let result = match_episodes_fingerprints_with(
            &shared_reference_occurrences(reverse, 0.2),
            &episodes(3),
            15.0,
            240.0,
        );
        assert_eq!(result.len(), 3);
        let target = result.iter().find(|v| v.episode == 1).unwrap();
        assert_eq!(
            (target.intro_start_ms, target.intro_end_ms),
            (90_000, 160_000)
        );
    }
}

#[test]
fn publication_needs_two_distinct_other_episodes() {
    let segment = common_segment((0.0, 70.0), (0.0, 70.0), 0.5);
    let engine = repeated(vec![segment.clone(), segment], 3);
    assert!(match_episodes_fingerprints_with(&engine, &episodes(2), 15.0, 240.0).is_empty());
    let duplicate_episodes = vec![(1, vec![1, 11]), (2, vec![2, 12]), (2, vec![3, 13])];
    assert!(match_episodes_fingerprints_with(&engine, &duplicate_episodes, 15.0, 240.0).is_empty());
    assert_eq!(
        match_episodes_fingerprints_with(&engine, &episodes(3), 15.0, 240.0).len(),
        3
    );
}

#[test]
fn constant_and_silent_fingerprints_are_not_published_even_by_an_injected_engine() {
    let engine = repeated(vec![common_segment((0.0, 70.0), (0.0, 70.0), 0.5)], 3);
    let constant = (1..=3).map(|n| (n, vec![n; 100])).collect::<Vec<_>>();
    assert!(match_episodes_fingerprints_with(&engine, &constant, 15.0, 240.0).is_empty());
    let silent = (1..=3)
        .map(|n| {
            let mut words = vec![0; 100];
            words[0] = n;
            (n, words)
        })
        .collect::<Vec<_>>();
    assert!(match_episodes_fingerprints_with(&engine, &silent, 15.0, 240.0).is_empty());
}

#[test]
fn invalid_coordinates_and_duration_are_not_published() {
    let good = common_segment((0.0, 70.0), (0.0, 70.0), 0.5);
    let mut invalid = Vec::new();
    let mut s = good.clone();
    s.start1_sec = f32::NAN;
    invalid.push(s);
    let mut s = good.clone();
    s.start1_sec = -1.0;
    invalid.push(s);
    let mut s = good.clone();
    s.end2_sec = 0.0;
    invalid.push(s);
    let mut s = good.clone();
    s.duration_sec = 300.0;
    invalid.push(s);
    for segment in invalid {
        let engine = repeated(vec![segment], 3);
        assert!(match_episodes_fingerprints_with(&engine, &episodes(3), 15.0, 240.0).is_empty());
    }
}

fn adaptive_context(
    reference_count: u32,
) -> (
    marker::EpisodeEvidence,
    TemplateContext,
    MockVerificationEngine,
) {
    let target = make_target_evidence("s1", 1, SegmentKind::Intro, 1, 0, 180_000);
    let mut templates = TemplateContext::default();
    let references = (1..=reference_count)
        .map(|n| {
            let id = format!("s{n}");
            templates.references.insert(
                id.clone(),
                make_target_evidence(&id, n, SegmentKind::Intro, n, 0, 180_000),
            );
            template_reference(&id, n, (0, 70_000))
        })
        .collect();
    templates
        .models
        .push(intro_template_model("intro", references, 70_000));
    let engine = repeated(
        vec![common_segment((0.0, 70.0), (0.0, 70.0), 0.5)],
        reference_count,
    );
    (target, templates, engine)
}

#[test]
fn adaptive_template_member_still_needs_two_other_episodes() {
    let policy = SamplingPolicy::default();
    let (target, templates, engine) = adaptive_context(2);
    assert!(matches!(
        verify_template_window(&engine, &target, &templates, &policy),
        VerificationOutcome::NeedsFullWindow { .. }
    ));
    let (target, templates, engine) = adaptive_context(3);
    assert!(
        matches!(verify_template_window(&engine, &target, &templates, &policy), VerificationOutcome::Verified(v) if v.start_ms == 0 && v.end_ms == 70_000 && v.supporting_episodes == 2)
    );
}

#[test]
fn adaptive_rejects_nonfinite_quality_and_silent_reference_evidence() {
    for score in [f64::NAN, f64::NEG_INFINITY] {
        let (target, templates, _) = adaptive_context(3);
        let engine = repeated(vec![common_segment((0.0, 70.0), (0.0, 70.0), score)], 3);
        assert!(matches!(
            verify_template_window(&engine, &target, &templates, &SamplingPolicy::default()),
            VerificationOutcome::NeedsFullWindow { .. }
        ));
    }
    let (target, mut templates, engine) = adaptive_context(3);
    for evidence in templates.references.values_mut() {
        evidence.capture.words = vec![evidence.episode; 100];
    }
    assert!(matches!(
        verify_template_window(&engine, &target, &templates, &SamplingPolicy::default()),
        VerificationOutcome::NeedsFullWindow { .. }
    ));
}

#[test]
fn weak_adjacent_audio_cannot_be_laundered_by_a_good_longer_segment() {
    let first = (0..1454u32)
        .map(|i| ((500 + i) << 20) | ((i * 7919 + 31) & 0x000f_ffff))
        .collect::<Vec<_>>();
    let second = first
        .iter()
        .enumerate()
        .map(|(i, word)| word ^ if i < 1000 { 0b111 } else { 0b11_1111 })
        .collect::<Vec<_>>();
    let segment = marker::find_common_segment(&first, &second, 15.0, 240.0)
        .expect("the good part remains a useful candidate");
    assert!(
        segment.end1_sec < 140.0,
        "weak tail must not be averaged into the ~180s interval: {segment:?}"
    );
}

#[test]
fn isolated_fingerprint_noise_keeps_a_high_quality_long_candidate() {
    let first = (0..1454u32)
        .map(|i| ((500 + i) << 20) | ((i * 7919 + 31) & 0x000f_ffff))
        .collect::<Vec<_>>();
    let second = first
        .iter()
        .enumerate()
        .map(|(i, word)| {
            word ^ if (1000..1003).contains(&i) {
                0b11_1111
            } else {
                0b111
            }
        })
        .collect::<Vec<_>>();
    let segment = marker::find_common_segment(&first, &second, 15.0, 240.0)
        .expect("isolated noise is within the policy gap budget");
    assert!((175.0..185.0).contains(&segment.duration_sec));
}

#[test]
fn adaptive_checks_all_alternatives_and_abstains_on_equal_quality() {
    for score in [3.9, 0.5] {
        let (target, templates, _) = adaptive_context(3);
        let engine = repeated(
            vec![
                common_segment((0.0, 70.0), (0.0, 70.0), score),
                common_segment((90.0, 160.0), (0.0, 70.0), 0.5),
            ],
            3,
        );
        let result =
            verify_template_window(&engine, &target, &templates, &SamplingPolicy::default());
        if score == 3.9 {
            assert!(
                matches!(result, VerificationOutcome::Verified(v) if v.start_ms == 90_000 && v.end_ms == 160_000 && v.supporting_episodes == 2)
            );
        } else {
            assert!(matches!(
                result,
                VerificationOutcome::NeedsFullWindow { .. }
            ));
        }
    }
}

#[test]
fn adaptive_keeps_true_outro_media_end_with_left_guard_evidence() {
    let (mut target, mut templates, _) = adaptive_context(3);
    target.kind = SegmentKind::Outro;
    target.duration_ms = Some(180_000);
    templates.models[0].kind = SegmentKind::Outro;
    for reference in templates.references.values_mut() {
        reference.kind = SegmentKind::Outro;
        reference.duration_ms = Some(180_000);
    }
    for reference in &mut templates.models[0].references {
        reference.match_interval_ms = (110_000, 180_000);
    }
    let engine = repeated(vec![common_segment((110.0, 180.0), (110.0, 180.0), 0.5)], 3);
    assert!(
        matches!(verify_template_window(&engine, &target, &templates, &SamplingPolicy::default()), VerificationOutcome::Verified(v) if v.start_ms == 110_000 && v.end_ms == 180_000)
    );
}

#[test]
fn nonfinite_scores_do_not_form_season_models() {
    let (_, templates, _) = adaptive_context(3);
    let evidence = templates.references.into_values().collect::<Vec<_>>();
    for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let engine = repeated(vec![common_segment((0.0, 70.0), (0.0, 70.0), score)], 3);
        assert!(
            marker::build_season_models(&engine, &evidence, &SamplingPolicy::default()).is_empty()
        );
    }
}
