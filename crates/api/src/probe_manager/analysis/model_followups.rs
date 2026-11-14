use super::*;

pub(super) fn prepare(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    inputs: &Inputs,
    sets: &[store::StoredFingerprintModelSet],
) -> Result<Vec<store::ProbeAnalysisFollowups>, store::StoreError> {
    if sets.is_empty() {
        return Ok(vec![]);
    }
    let replacement = sets
        .iter()
        .flat_map(|set| &set.bundles)
        .map(|bundle| {
            serde_json::from_str::<marker::adaptive::TemplateModel>(&bundle.model.model_json)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let previous = &inputs.templates.models;
    let equivalent = previous
        .iter()
        .all(|old| replacement.iter().any(|new| same_theme(old, new)))
        && replacement
            .iter()
            .all(|new| previous.iter().any(|old| same_theme(old, new)));
    if equivalent {
        return Ok(vec![]);
    }
    let new_ids = replacement
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut candidates = Vec::new();
    for reference in &inputs.references {
        if claim
            .participants
            .iter()
            .any(|p| p.dispatch().ledger_id == reference.dispatch().ledger_id)
        {
            continue;
        }
        let id = &reference.dispatch().ledger_id;
        let Some(job) = manager.store.latest_probe_job_for_ledger(id)? else {
            continue;
        };
        let units = manager.store.probe_job_units(&job)?;
        let Some(raw) = units
            .iter()
            .find(|unit| &unit.ledger_id == id)
            .and_then(|unit| unit.detection_outcome_json.as_deref())
        else {
            continue;
        };
        let outcome: serde_json::Value = serde_json::from_str(raw)?;
        let old_ids = outcome["model_reference_key"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if (outcome["status"] == "verified" || outcome["status"] == "partial") && old_ids != new_ids
        {
            candidates.push(reference.clone());
        }
    }
    candidates
        .chunks(16)
        .map(|page| {
            store::ProbeAnalysisFollowups::new(
                claim.participants[0].clone(),
                page.to_vec(),
                inputs.reference_key.clone(),
            )
        })
        .collect()
}

fn same_theme(
    old: &marker::adaptive::TemplateModel,
    new: &marker::adaptive::TemplateModel,
) -> bool {
    old.kind == new.kind
        && old.expected_duration_ms == new.expected_duration_ms
        && old.is_stable == new.is_stable
        && old.references.iter().all(|prior| {
            new.references.iter().any(|current| {
                current.sample_id == prior.sample_id
                    && current.ledger_id == prior.ledger_id
                    && current.episode == prior.episode
                    && current.source_version == prior.source_version
                    && current.match_interval_ms == prior.match_interval_ms
                    && current.match_from_end_ms == prior.match_from_end_ms
            })
        })
}
