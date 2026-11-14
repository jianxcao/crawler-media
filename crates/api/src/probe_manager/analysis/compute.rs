use super::*;
use marker::matcher::FingerprintDecision;
use std::collections::HashMap;
use store::{ProbeAnalysisCommit, ProbeAnalysisOutput, ProbeAnalysisParticipant};
#[path = "adaptive.rs"]
mod adaptive;
#[path = "inputs.rs"]
mod inputs;
#[path = "model_followups.rs"]
mod model_followups;
#[path = "output.rs"]
mod output;
use inputs::load_inputs;
use output::output;

struct Inputs {
    evidence: Vec<marker::EpisodeEvidence>,
    references: Vec<ProbeAnalysisParticipant>,
    // Same-read raw guards qualify terminal targets even when no capture is usable.
    observed: Vec<ProbeAnalysisParticipant>,
    chapters: HashMap<String, Vec<library::ChapterMarker>>,
    reference_key: String,
    templates: marker::adaptive::TemplateContext,
}

pub(super) fn run(
    manager: &ProbeManager,
    key: SeasonKey,
) -> Result<Option<(String, bool)>, store::StoreError> {
    let participants =
        manager
            .store
            .probe_analysis_participants(key.media_id, key.season, None, 16)?;
    for participant in &participants {
        if let Err(reason) =
            crate::fingerprint_job::source::observe_source(&participant.context().target)
        {
            if crate::fingerprint_job::source::is_file_deleted(&reason) {
                manager.cancel_ledger_work(&[participant.dispatch().ledger_id.clone()], &reason)?;
            } else {
                return Err(store::StoreError::Invalid(reason));
            }
        }
    }
    let participants =
        manager
            .store
            .probe_analysis_participants(key.media_id, key.season, None, 16)?;
    if participants.is_empty() {
        return Ok(None);
    }
    for participant in &participants {
        if manager
            .store
            .get_probe_job(&participant.dispatch().job_id)?
            .is_some_and(|j| j.kind == "marker_refresh")
        {
            refresh(manager, &participant.dispatch().job_id)?;
            return Ok(None);
        }
    }
    let inputs = load_inputs(manager, key, &participants[0])?;
    reconcile(manager, &participants[0], &inputs)?;
    let participants = observed_participants(manager, key, participants[0].context(), &inputs)?;
    let Some(claim) = manager
        .store
        .claim_probe_analysis(&uuid::Uuid::new_v4().to_string(), &participants)?
    else {
        return Ok(None);
    };
    let prepared = prepare(manager, &claim, &inputs);
    let result = prepared.and_then(|commit| publish(manager, &commit));
    if result.is_err() {
        manager.store.requeue_probe_analysis(&claim)?;
    }
    result?;
    let pending = manager
        .store
        .probe_analysis_participants(key.media_id, key.season, None, 16)?;
    if pending.is_empty() {
        return Ok(None);
    }
    let current = load_inputs(manager, key, &pending[0])?;
    let digest = pending
        .iter()
        .map(|p| p.context().context_key.as_str())
        .collect::<Vec<_>>()
        .join("|");
    Ok(Some((
        digest,
        current.reference_key == inputs.reference_key,
    )))
}

fn observed_participants(
    manager: &ProbeManager,
    key: SeasonKey,
    anchor: &store::ProbeAnalysisContext,
    inputs: &Inputs,
) -> Result<Vec<ProbeAnalysisParticipant>, store::StoreError> {
    let participants = manager
        .store
        .probe_analysis_participants(key.media_id, key.season, None, 16)?
        .into_iter()
        .filter(|p| {
            p.context().sampling_mode == anchor.sampling_mode
                && p.context().analysis_policy_key == anchor.analysis_policy_key
                && inputs.observed.iter().any(|current| {
                    let observed_samples = current
                        .context()
                        .evidence
                        .iter()
                        .filter_map(|(id, _)| id.strip_prefix("sample:"))
                        .collect::<Vec<_>>();
                    p.observed_by(current, &observed_samples)
                })
        })
        .collect::<Vec<_>>();
    tracing::debug!(media_id=%key.media_id,season=key.season,participants=participants.len(),references=inputs.references.len(),reference_key=%inputs.reference_key,"analysis observed input selection");
    for participant in &participants {
        if manager
            .store
            .probe_analysis_refresh_state(participant.dispatch())?
            .is_some_and(|state| {
                !state.reconcile_done || state.wanted_reference_key != inputs.reference_key
            })
        {
            reconcile(manager, participant, &inputs)?;
        }
    }
    Ok(participants)
}

fn refresh(manager: &ProbeManager, job_id: &str) -> Result<(), store::StoreError> {
    if let Some(job) = manager
        .store
        .fail_probe_analysis_refresh_if_capture_failed(job_id)?
    {
        super::super::timings::finalize_terminal_job(
            &manager.store_access(),
            &manager.timings,
            &job,
        );
        return Ok(());
    }
    let Some(participants) = manager.store.probe_analysis_job_participants(job_id)? else {
        return Ok(());
    };
    let Some(claim) = manager
        .store
        .claim_probe_analysis(&uuid::Uuid::new_v4().to_string(), &participants)?
    else {
        return Ok(());
    };
    let result = (|| {
        let mut combined = ProbeAnalysisCommit {
            claim: claim.clone(),
            outputs: vec![],
            references: vec![],
            observations: vec![],
            models: vec![],
            followups: vec![],
            refresh_job_id: Some(job_id.into()),
            refresh_error: None,
        };
        let seasons = participants
            .iter()
            .map(|p| p.context().season)
            .collect::<std::collections::BTreeSet<_>>();
        for season in seasons {
            let selected = participants
                .iter()
                .filter(|p| p.context().season == season)
                .cloned()
                .collect::<Vec<_>>();
            let key = SeasonKey {
                media_id: selected[0].context().media_id,
                season,
            };
            let inputs = load_inputs(manager, key, &selected[0])?;
            let subclaim = store::ProbeAnalysisClaim {
                token: claim.token.clone(),
                participants: selected,
            };
            let prepared = prepare(manager, &subclaim, &inputs)?;
            combined.outputs.extend(prepared.outputs);
            combined.references.extend(prepared.references);
            combined.models.extend(prepared.models);
            combined.followups.extend(prepared.followups);
            for observation in prepared.observations {
                if !combined
                    .observations
                    .iter()
                    .any(|old| old.file_id == observation.file_id)
                {
                    combined.observations.push(observation);
                }
            }
        }
        publish(manager, &combined)
    })();
    if result.is_err() {
        manager.store.requeue_probe_analysis(&claim)?;
    }
    result
}

fn publish(manager: &ProbeManager, commit: &ProbeAnalysisCommit) -> Result<(), store::StoreError> {
    loop {
        match manager.store.complete_probe_analysis_checked(commit) {
            Ok(Some(jobs)) => {
                if let Some(frames) = manager.chapter_frames() {
                    frames.notify_pending();
                }
                for job in jobs {
                    super::super::timings::finalize_terminal_job(
                        &manager.store_access(),
                        &manager.timings,
                        &job,
                    );
                }
                return Ok(());
            }
            Ok(None) => {
                return Err(store::StoreError::Protected(
                    "analysis input or claim changed".into(),
                ));
            }
            Err(error)
                if super::super::store_access::is_busy(&error)
                    && !manager.shutting_down.load(Ordering::SeqCst) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

fn reconcile(
    manager: &ProbeManager,
    trigger: &ProbeAnalysisParticipant,
    inputs: &Inputs,
) -> Result<(), store::StoreError> {
    let state = manager
        .store
        .probe_analysis_refresh_state(trigger.dispatch())?;
    let unchanged = state
        .as_ref()
        .is_some_and(|s| s.wanted_reference_key == inputs.reference_key);
    if unchanged && state.as_ref().is_some_and(|s| s.reconcile_done) {
        return Ok(());
    }
    let mut expected = state.and_then(|state| state.reconcile_after);
    let mut cursor = if unchanged {
        expected
            .as_deref()
            .map(serde_json::from_str::<store::ProbeAnalysisCursor>)
            .transpose()?
    } else {
        None
    };
    loop {
        let page = inputs
            .references
            .iter()
            .filter(|p| {
                cursor
                    .as_ref()
                    .is_none_or(|after| coverage_cursor(p) > *after)
            })
            .take(16)
            .cloned()
            .collect::<Vec<_>>();
        let next = page
            .last()
            .filter(|_| page.len() == 16)
            .map(|last| serde_json::to_string(&coverage_cursor(last)))
            .transpose()?;
        let mut candidates = Vec::new();
        for candidate in &page {
            if candidate.dispatch().ledger_id == trigger.dispatch().ledger_id {
                continue;
            }
            let Some(job) = manager
                .store
                .latest_probe_job_for_ledger(&candidate.dispatch().ledger_id)?
            else {
                continue;
            };
            let units = manager.store.probe_job_units(&job)?;
            if units
                .iter()
                .filter(|unit| unit.ledger_id == candidate.dispatch().ledger_id)
                .any(|unit| {
                    unit.detection_outcome_json
                        .as_deref()
                        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                        .is_some_and(|value| {
                            (value["status"] == "uncertain" || value["status"] == "partial")
                                || (trigger.context().sampling_mode == "full_window"
                                    && value["available_reference_key"].as_str()
                                        != Some(inputs.reference_key.as_str()))
                                || value["reference_evidence"].as_array().is_some_and(|old| {
                                    old.iter().any(|reference| {
                                        !inputs.references.iter().any(|current| {
                                            reference[0].as_str()
                                                == Some(current.dispatch().ledger_id.as_str())
                                                && reference[1].as_str()
                                                    == Some(current.context().context_key.as_str())
                                        })
                                    })
                                })
                        })
                })
            {
                candidates.push(candidate.clone());
            }
        }
        if !manager
            .store
            .refresh_probe_analysis_targets(&store::ProbeAnalysisReconcile {
                trigger: trigger.clone(),
                expected_cursor: expected,
                reference_key: inputs.reference_key.clone(),
                candidates,
                next_cursor: next.clone(),
            })?
        {
            return Err(store::StoreError::Protected(
                "analysis reconciliation changed".into(),
            ));
        }
        if next.is_none() {
            return Ok(());
        }
        cursor = next.as_deref().map(serde_json::from_str).transpose()?;
        expected = next;
    }
}

fn coverage_cursor(participant: &ProbeAnalysisParticipant) -> store::ProbeAnalysisCursor {
    let row = &participant.context().target;
    store::ProbeAnalysisCursor(
        row.units
            .iter()
            .find(|u| u.season == participant.context().season as i32)
            .map(|u| u.episode)
            .unwrap_or(-1),
        row.file.library_id.to_string(),
        row.file.file_id.to_string(),
    )
}

fn prepare(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    inputs: &Inputs,
) -> Result<ProbeAnalysisCommit, store::StoreError> {
    let chapters = inputs
        .chapters
        .iter()
        .map(|(id, chapters)| {
            (
                id.clone(),
                chapters
                    .iter()
                    .filter(|c| !c.synthetic)
                    .map(|c| marker::boundary::Interval {
                        start_ms: c.start_ms,
                        end_ms: c.end_ms,
                    })
                    .collect(),
            )
        })
        .collect();
    let policy = serde_json::from_str(&claim.participants[0].context().analysis_policy_json)?;
    let (decisions, models, used) = if claim.participants[0].context().sampling_mode == "adaptive" {
        adaptive::decisions(manager, claim, inputs, &chapters, &policy)?
    } else {
        let (decisions, measurement) =
            crate::fingerprint_job::measurement::EvidenceMeasurement::new(
                manager.fingerprint_engine.clone(),
                &policy,
            )
            .measure(|engine| {
                marker::matcher::verify_season_fingerprints_with(
                    engine,
                    &inputs.evidence,
                    &chapters,
                    &policy,
                )
            });
        manager.comparison_runs.fetch_add(1, Ordering::Relaxed);
        record_measurement(manager, claim, &measurement);
        (decisions, vec![], None)
    };
    let references = inputs
        .references
        .iter()
        .filter(|p| {
            used.as_ref()
                .is_none_or(|used| used.contains(&p.dispatch().ledger_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    let model_reference_key = if !models.is_empty() {
        models
            .iter()
            .flat_map(|set| &set.bundles)
            .map(|bundle| bundle.model.model_id.clone())
            .collect::<Vec<_>>()
    } else {
        inputs
            .templates
            .models
            .iter()
            .map(|model| model.model_id.clone())
            .collect::<Vec<_>>()
    };
    let outputs = claim
        .participants
        .iter()
        .map(|participant| {
            output(
                manager,
                participant,
                inputs,
                &decisions,
                &references,
                &model_reference_key,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut reference_rows = references
        .iter()
        .map(|p| p.context().target.clone())
        .collect::<Vec<_>>();
    // Rejected evidence still needs a current source observation for terminal CAS.
    for participant in &claim.participants {
        let target = &participant.context().target;
        if !reference_rows
            .iter()
            .any(|row| row.file.file_id == target.file.file_id)
        {
            reference_rows.push(target.clone());
        }
    }
    let observations = super::super::markers::source_observations(&reference_rows)?;
    let followups = model_followups::prepare(manager, claim, inputs, &models)?;
    Ok(ProbeAnalysisCommit {
        claim: claim.clone(),
        outputs,
        references,
        observations,
        models,
        followups,
        refresh_job_id: None,
        refresh_error: None,
    })
}

fn record_measurement(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    measurement: &crate::fingerprint_job::measurement::ComparisonMeasurement,
) {
    // One actual invocation has one owner. Other participating jobs record a
    // known zero contribution, so Media aggregation never multiplies shared CPU.
    let jobs = claim
        .participants
        .iter()
        .map(|p| p.dispatch().job_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let shared = Default::default();
    for (index, job) in jobs.into_iter().enumerate() {
        manager
            .timings
            .record_analysis(Some(job), if index == 0 { measurement } else { &shared });
    }
}
