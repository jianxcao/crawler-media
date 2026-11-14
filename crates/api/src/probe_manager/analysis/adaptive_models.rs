//! Bounded discovery views: one actual sample per independent episode, never joined PCM.
use super::*;
use marker::{EpisodeEvidence, SegmentKind, TemplateModel};

pub(super) fn geometry(e: &EpisodeEvidence) -> (u8, i64, i64) {
    let w = &e.capture.window;
    match e.kind {
        SegmentKind::Intro => (0, w.start_ms, w.end_ms),
        SegmentKind::Outro => (
            1,
            e.duration_ms.unwrap_or(0) - w.end_ms,
            e.duration_ms.unwrap_or(0) - w.start_ms,
        ),
    }
}

pub(super) fn build(
    engine: &dyn marker::FingerprintEngine,
    inputs: &Inputs,
    policy: &marker::SamplingPolicy,
) -> Vec<TemplateModel> {
    let mut models = Vec::new();
    for kind in [SegmentKind::Intro, SegmentKind::Outro] {
        let samples = bounded_samples(inputs, kind, policy);
        // At most max_seed_count * max_windows_per_kind anchors (default 15).
        // Each view chooses at most one matching sample for each of five episodes.
        for anchor in &samples {
            let view = discovery_view(engine, anchor, &samples, policy);
            for mut model in marker::adaptive::build_season_models(engine, &view, policy) {
                if !model.is_stable || !guarded(&model, &view, policy) {
                    continue;
                }
                if models
                    .iter()
                    .any(|old: &TemplateModel| same_references(old, &model))
                {
                    continue;
                }
                let count = models.iter().filter(|m| m.kind == kind).count();
                if count >= policy.max_templates_per_kind {
                    break;
                }
                model.model_id = format!("{}-view-{count}", model.model_id);
                models.push(model);
            }
        }
    }
    models
}

fn bounded_samples(
    inputs: &Inputs,
    kind: SegmentKind,
    policy: &marker::SamplingPolicy,
) -> Vec<EpisodeEvidence> {
    let mut episodes = std::collections::BTreeMap::<u32, Vec<&EpisodeEvidence>>::new();
    for e in inputs
        .evidence
        .iter()
        .chain(inputs.templates.references.values())
        .filter(|e| e.kind == kind)
    {
        if e.capture.window.duration_ms() <= 300_000
            && marker::boundary::evidence::candidate_pcm_coverage(&e.capture).is_some()
        {
            episodes.entry(e.episode).or_default().push(e);
        }
    }
    episodes
        .into_values()
        .take(policy.max_seed_count)
        .flat_map(|mut samples| {
            samples.sort_by(|a, b| {
                a.capture
                    .window
                    .start_ms
                    .cmp(&b.capture.window.start_ms)
                    .then(a.sample_id.cmp(&b.sample_id))
            });
            samples.dedup_by(|a, b| a.sample_id == b.sample_id);
            samples
                .into_iter()
                .take(policy.max_windows_per_kind)
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

fn discovery_view(
    engine: &dyn marker::FingerprintEngine,
    anchor: &EpisodeEvidence,
    samples: &[EpisodeEvidence],
    policy: &marker::SamplingPolicy,
) -> Vec<EpisodeEvidence> {
    let mut view = vec![anchor.clone()];
    let mut episodes = HashSet::from([anchor.episode]);
    for candidate in samples {
        if episodes.contains(&candidate.episode) {
            continue;
        }
        if matches!(engine.compare_evidence(anchor,candidate,policy.min_match_duration_ms as f32/1000.,policy.max_match_duration_ms as f32/1000.),marker::PairFingerprintDecision::Candidates(candidates) if !candidates.is_empty())
        {
            episodes.insert(candidate.episode);
            view.push(candidate.clone());
        }
    }
    view
}

fn guarded(
    model: &TemplateModel,
    evidence: &[EpisodeEvidence],
    policy: &marker::SamplingPolicy,
) -> bool {
    model.references.iter().all(|r| {
        evidence
            .iter()
            .find(|e| e.sample_id == r.sample_id)
            .is_some_and(|e| {
                marker::boundary::evidence::complete_guards(
                    &e.capture,
                    marker::boundary::Interval {
                        start_ms: r.match_interval_ms.0,
                        end_ms: r.match_interval_ms.1,
                    },
                    model.kind,
                    e.duration_ms,
                    &policy.boundary,
                )
            })
    })
}
fn same_references(a: &TemplateModel, b: &TemplateModel) -> bool {
    a.kind == b.kind
        && a.expected_duration_ms == b.expected_duration_ms
        && a.references.len() == b.references.len()
        && a.references.iter().all(|r| {
            b.references.iter().any(|other| {
                r.sample_id == other.sample_id && r.match_interval_ms == other.match_interval_ms
            })
        })
}
