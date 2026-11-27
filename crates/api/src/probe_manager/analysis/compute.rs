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

fn is_marker_refresh_job(
    manager: &ProbeManager,
    cache: &mut HashMap<String, bool>,
    job_id: &str,
) -> Result<bool, store::StoreError> {
    if let Some(&cached) = cache.get(job_id) {
        return Ok(cached);
    }
    let is_refresh = manager
        .store
        .get_probe_job(job_id)?
        .is_some_and(|job| job.kind == "marker_refresh");
    cache.insert(job_id.to_string(), is_refresh);
    Ok(is_refresh)
}

fn observe_season_sources(manager: &ProbeManager, key: SeasonKey) -> Result<(), store::StoreError> {
    let mut after: Option<(String, String)> = None;
    loop {
        let page = manager.store.probe_analysis_participants(
            key.media_id,
            key.season,
            after.as_ref().map(|(j, f)| (j.as_str(), f.as_str())),
            16,
        )?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = Some((
            last.dispatch().job_id.clone(),
            last.dispatch().ledger_id.clone(),
        ));
        for participant in &page {
            if let Err(reason) =
                crate::fingerprint_job::source::observe_source(&participant.context().target)
            {
                if crate::fingerprint_job::source::is_file_deleted(&reason) {
                    manager.cancel_ledger_work(
                        std::slice::from_ref(&participant.dispatch().ledger_id),
                        &reason,
                    )?;
                } else {
                    return Err(store::StoreError::Invalid(reason));
                }
            }
        }
    }
}

fn collect_eligible_non_refresh(
    manager: &ProbeManager,
    key: SeasonKey,
    job_kinds: &mut HashMap<String, bool>,
    run_refresh: bool,
) -> Result<(bool, Vec<ProbeAnalysisParticipant>), store::StoreError> {
    let mut after: Option<(String, String)> = None;
    let mut tried_refresh = std::collections::HashSet::new();
    let mut eligible = Vec::new();
    let paused = manager.fingerprint_dispatch_paused();
    loop {
        let page = manager.store.probe_analysis_participants(
            key.media_id,
            key.season,
            after.as_ref().map(|(j, f)| (j.as_str(), f.as_str())),
            16,
        )?;
        let Some(last) = page.last() else {
            return Ok((false, eligible));
        };
        after = Some((
            last.dispatch().job_id.clone(),
            last.dispatch().ledger_id.clone(),
        ));
        for participant in page {
            let job_id = &participant.dispatch().job_id;
            if is_marker_refresh_job(manager, job_kinds, job_id)? {
                if run_refresh && tried_refresh.insert(job_id.clone()) && refresh(manager, job_id)?
                {
                    return Ok((true, Vec::new()));
                }
                continue;
            }
            if eligible.len() < 16
                && (!paused
                    || manager.is_dispatch_paused_exempt(job_id, &participant.dispatch().ledger_id))
            {
                eligible.push(participant);
            }
        }
        if !run_refresh && eligible.len() >= 16 {
            return Ok((false, eligible));
        }
    }
}

pub(super) fn run(
    manager: &ProbeManager,
    key: SeasonKey,
) -> Result<Option<(String, bool)>, store::StoreError> {
    observe_season_sources(manager, key)?;
    let mut job_kinds = HashMap::new();
    let (refresh_handled, eligible) =
        collect_eligible_non_refresh(manager, key, &mut job_kinds, true)?;
    if refresh_handled || eligible.is_empty() {
        return Ok(None);
    }
    let anchor = &eligible[0];
    let inputs = load_inputs(manager, key, anchor)?;
    reconcile(manager, anchor, &inputs)?;
    let mut participants =
        observed_participants(manager, key, anchor.context(), &inputs, &mut job_kinds)?;
    if manager.fingerprint_dispatch_paused() {
        participants.retain(|p| {
            manager.is_dispatch_paused_exempt(&p.dispatch().job_id, &p.dispatch().ledger_id)
        });
    }
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
    let (_, pending) = collect_eligible_non_refresh(manager, key, &mut job_kinds, false)?;
    if pending.is_empty() {
        return Ok(None);
    }
    let current = load_inputs(manager, key, &pending[0])?;
    let Some(digest) = super::season_analysis_digest(manager, key.media_id, key.season)? else {
        return Ok(None);
    };
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
    job_kinds: &mut HashMap<String, bool>,
) -> Result<Vec<ProbeAnalysisParticipant>, store::StoreError> {
    let mut participants = Vec::new();
    let mut after: Option<(String, String)> = None;
    let paused = manager.fingerprint_dispatch_paused();
    while participants.len() < 16 {
        let page = manager.store.probe_analysis_participants(
            key.media_id,
            key.season,
            after.as_ref().map(|(j, f)| (j.as_str(), f.as_str())),
            16,
        )?;
        let Some(last) = page.last() else {
            break;
        };
        after = Some((
            last.dispatch().job_id.clone(),
            last.dispatch().ledger_id.clone(),
        ));
        for p in page {
            if participants.len() >= 16 {
                break;
            }
            if is_marker_refresh_job(manager, job_kinds, &p.dispatch().job_id)? {
                continue;
            }
            if paused
                && !manager.is_dispatch_paused_exempt(&p.dispatch().job_id, &p.dispatch().ledger_id)
            {
                continue;
            }
            let matches_anchor = p.context().sampling_mode == anchor.sampling_mode
                && p.context().analysis_policy_key == anchor.analysis_policy_key
                && inputs.observed.iter().any(|current| {
                    let observed_samples = current
                        .context()
                        .evidence
                        .iter()
                        .filter_map(|(id, _)| id.strip_prefix("sample:"))
                        .collect::<Vec<_>>();
                    p.observed_by(current, &observed_samples)
                });
            if matches_anchor {
                participants.push(p);
            }
        }
    }
    tracing::debug!(media_id=%key.media_id,season=key.season,participants=participants.len(),references=inputs.references.len(),reference_key=%inputs.reference_key,"analysis observed input selection");
    for participant in &participants {
        if manager
            .store
            .probe_analysis_refresh_state(participant.dispatch())?
            .is_some_and(|state| {
                !state.reconcile_done || state.wanted_reference_key != inputs.reference_key
            })
        {
            reconcile(manager, participant, inputs)?;
        }
    }
    Ok(participants)
}

fn clear_queued_analysis_unit_exemptions(
    manager: &ProbeManager,
    job_id: &str,
) -> Result<(), store::StoreError> {
    for (exempt_job, lid) in manager.paused_exempt_unit_pairs() {
        if exempt_job != job_id {
            continue;
        }
        if manager
            .store
            .active_probe_unit_for_ledger(&lid)?
            .is_some_and(|u| {
                u.job_id == job_id
                    && u.stage == store::ProbeDispatchStage::Analysis
                    && u.status == "queued"
            })
        {
            manager.unmark_unit_paused_exempt(job_id, &lid);
        }
    }
    Ok(())
}

fn refresh(manager: &ProbeManager, job_id: &str) -> Result<bool, store::StoreError> {
    if let Some(job) = manager
        .store
        .fail_probe_analysis_refresh_if_capture_failed(job_id)?
    {
        manager.unmark_job_paused_exempt(&job.id);
        super::super::timings::finalize_terminal_job(
            &manager.store_access(),
            &manager.timings,
            &job,
        );
        return Ok(true);
    }
    let Some(participants) = manager.store.probe_analysis_job_participants(job_id)? else {
        if manager.fingerprint_dispatch_paused() && !manager.is_job_paused_exempt(job_id) {
            clear_queued_analysis_unit_exemptions(manager, job_id)?;
        }
        return Ok(false);
    };
    if manager.fingerprint_dispatch_paused()
        && !manager.is_job_paused_exempt(job_id)
        && !participants.iter().any(|p| {
            let lid = &p.dispatch().ledger_id;
            manager.is_unit_paused_exempt(job_id, lid) || manager.was_unit_promoted(job_id, lid)
        })
    {
        return Ok(false);
    }
    let Some(claim) = manager
        .store
        .claim_probe_analysis(&uuid::Uuid::new_v4().to_string(), &participants)?
    else {
        return Ok(false);
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
    result.map(|()| true)
}

fn publish(manager: &ProbeManager, commit: &ProbeAnalysisCommit) -> Result<(), store::StoreError> {
    loop {
        match manager.store.complete_probe_analysis_checked(commit) {
            Ok(Some(jobs)) => {
                if let Some(frames) = manager.chapter_frames() {
                    frames.notify_pending();
                }
                for participant in &commit.claim.participants {
                    let jid = &participant.dispatch().job_id;
                    let lid = &participant.dispatch().ledger_id;
                    manager.unmark_unit_paused_exempt(jid, lid);
                    manager.unmark_unit_promoted(jid, lid);
                }
                for job in jobs {
                    manager.unmark_job_paused_exempt(&job.id);
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
