use super::*;
use marker::adaptive::{TemplateContext, VerificationOutcome};
use std::collections::HashSet;
#[path = "adaptive_models.rs"]
mod model_groups;
#[path = "adaptive_search.rs"]
mod search;
type ResultSet = (
    Vec<marker::matcher::EpisodeFingerprintDecision>,
    Vec<store::StoredFingerprintModelSet>,
    Option<HashSet<String>>,
);

pub(super) fn decisions(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    inputs: &Inputs,
    chapters: &HashMap<String, Vec<marker::boundary::Interval>>,
    policy: &marker::SamplingPolicy,
) -> Result<ResultSet, store::StoreError> {
    let targets = claim
        .participants
        .iter()
        .map(|p| p.dispatch().ledger_id.as_str())
        .collect::<HashSet<_>>();
    let target_evidence = inputs
        .evidence
        .iter()
        .filter(|e| {
            claim.participants.iter().any(|p| {
                p.dispatch().ledger_id == e.ledger_id
                    && p.context()
                        .evidence
                        .iter()
                        .any(|(id, _)| id.strip_prefix("sample:") == Some(e.sample_id.as_str()))
            })
        })
        .collect::<Vec<_>>();
    let mut templates = inputs.templates.clone();
    let mut decisions = verify(
        manager,
        claim,
        &target_evidence,
        &templates,
        chapters,
        policy,
    );
    let mut bundles = Vec::new();
    let rebuilt = templates.models.is_empty()
        || decisions
            .iter()
            .any(|d| matches!(d.decision, FingerprintDecision::Uncertain(_)));
    if rebuilt {
        (templates, bundles) = rebuild_templates(manager, claim, inputs, &templates, policy)?;
        decisions = verify(
            manager,
            claim,
            &target_evidence,
            &templates,
            chapters,
            policy,
        );
    }
    decisions = search::reduce(decisions, &target_evidence, policy);
    let fallback = search::request_next(
        manager,
        claim,
        &mut decisions,
        &target_evidence,
        inputs,
        policy,
        templates
            .models
            .iter()
            .any(|model| model.kind == marker::SegmentKind::Intro && model.is_stable),
    )?;
    if fallback {
        manager.wake.notify_waiters();
        return Err(store::StoreError::Protected(
            "analysis needs bounded target capture".into(),
        ));
    }
    let mut used = targets
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for reference in templates.references.values() {
        used.insert(reference.ledger_id.clone());
    }
    let context = claim.participants[0].context();
    let sets = if rebuilt {
        vec![store::StoredFingerprintModelSet::new(
            context.media_id,
            context.season,
            context.analysis_policy_key.clone(),
            bundles,
        )?]
    } else {
        vec![]
    };
    Ok((decisions, sets, (!rebuilt).then_some(used)))
}

fn rebuild_templates(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    inputs: &Inputs,
    templates: &TemplateContext,
    policy: &marker::SamplingPolicy,
) -> Result<(TemplateContext, Vec<store::StoredFingerprintModelBundle>), store::StoreError> {
    let (models, measurement) = crate::fingerprint_job::measurement::EvidenceMeasurement::new(
        manager.fingerprint_engine.clone(),
        policy,
    )
    .measure(|engine| model_groups::build(engine, inputs, policy));
    record_measurement(manager, claim, &measurement);
    let rebuilt_kinds = models.iter().map(|m| m.kind).collect::<HashSet<_>>();
    let mut combined = models;
    combined.extend(
        templates
            .models
            .iter()
            .filter(|m| !rebuilt_kinds.contains(&m.kind))
            .cloned(),
    );
    let mut templates = TemplateContext::default();
    let mut bundles = Vec::new();
    for model in combined {
        let samples = model
            .references
            .iter()
            .map(|reference| {
                manager
                    .store
                    .fingerprint_sample_by_id(&reference.sample_id)?
                    .ok_or_else(|| store::StoreError::Missing("model sample disappeared".into()))
            })
            .collect::<Result<Vec<_>, store::StoreError>>()?;
        let first = claim.participants[0].context();
        let bundle = store::StoredFingerprintModelBundle::new(
            first.media_id,
            first.season,
            first.analysis_policy_key.clone(),
            model,
            samples,
            super::super::super::now_ms(),
        )?;
        let canonical: marker::adaptive::TemplateModel =
            serde_json::from_str(&bundle.model.model_json)?;
        let bundle = manager.store.bind_fingerprint_model(bundle)?;
        for reference in &canonical.references {
            if let Some(evidence) = inputs
                .evidence
                .iter()
                .chain(inputs.templates.references.values())
                .find(|e| e.sample_id == reference.sample_id)
            {
                templates
                    .references
                    .insert(reference.sample_id.clone(), evidence.clone());
            }
        }
        templates.models.push(canonical);
        bundles.push(bundle);
    }
    Ok((templates, bundles))
}

fn verify(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    targets: &[&marker::EpisodeEvidence],
    templates: &TemplateContext,
    chapters: &HashMap<String, Vec<marker::boundary::Interval>>,
    policy: &marker::SamplingPolicy,
) -> Vec<marker::matcher::EpisodeFingerprintDecision> {
    let (decisions, measurement) = crate::fingerprint_job::measurement::EvidenceMeasurement::new(
        manager.fingerprint_engine.clone(),
        policy,
    )
    .measure(|engine| {
        targets
            .iter()
            .map(|evidence| {
                let outcome = marker::adaptive::verify_template_window_with_chapters(
                    engine,
                    evidence,
                    templates,
                    policy,
                    chapters
                        .get(&evidence.ledger_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                );
                let decision = match outcome {
                    VerificationOutcome::Verified(interval) => {
                        FingerprintDecision::Verified(marker::boundary::Interval {
                            start_ms: interval.start_ms,
                            end_ms: interval.end_ms,
                        })
                    }
                    VerificationOutcome::NoMatch { .. } => FingerprintDecision::NoMatch,
                    VerificationOutcome::NeedsFullWindow { reason }
                    | VerificationOutcome::SamplingLimit { reason } => {
                        FingerprintDecision::Uncertain(reason)
                    }
                };
                marker::matcher::EpisodeFingerprintDecision {
                    sample_id: evidence.sample_id.clone(),
                    ledger_id: evidence.ledger_id.clone(),
                    episode: evidence.episode,
                    kind: evidence.kind,
                    window: evidence.capture.window.clone(),
                    decision,
                }
            })
            .collect()
    });
    record_measurement(manager, claim, &measurement);
    decisions
}
