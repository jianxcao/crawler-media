mod common;

use common::{MockVerificationEngine, common_segment, make_target_evidence};
use marker::{SamplingPolicy, SegmentKind, build_season_models};

fn models(origin: Option<i64>, pcm: i64) -> Vec<marker::TemplateModel> {
    let mut engine = MockVerificationEngine::default();
    for (a, b) in [(1000, 2000), (1000, 3000), (2000, 3000)] {
        engine.returns.insert(
            (a, b),
            vec![common_segment((30.0, 90.0), (30.0, 90.0), 1.0)],
        );
    }
    let inputs = (1..=3)
        .map(|episode| {
            let mut e = make_target_evidence(
                &format!("sample-{episode}"),
                episode,
                SegmentKind::Intro,
                episode * 1000,
                0,
                180_000,
            );
            e.capture.pcm_duration_ms = Some(pcm);
            e.capture.metrics.contiguous_pcm_start_ms = origin;
            e
        })
        .collect::<Vec<_>>();
    build_season_models(&engine, &inputs, &SamplingPolicy::default())
}

#[test]
fn proven_local_pcm_can_reach_existing_template_modeling() {
    let result = models(Some(0), 100_000);
    assert_eq!(
        result.len(),
        1,
        "covered candidate must reach later guard verification"
    );
    assert!(result[0].is_stable);
    assert_eq!(result[0].references.len(), 3);
}

#[test]
fn modeling_does_not_ignore_unknown_or_contradictory_alignment() {
    assert!(models(None, 100_000).is_empty());
    assert!(models(Some(5_000), 180_000).is_empty());
    assert_eq!(
        models(None, 180_000).len(),
        1,
        "complete unknown-origin contract is retained"
    );
}
