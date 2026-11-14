use super::super::super::intro_search::{self, KindSearch};
use super::*;
use marker::{SegmentKind, matcher::EpisodeFingerprintDecision};

pub(super) fn reduce(
    decisions: Vec<EpisodeFingerprintDecision>,
    target_evidence: &[&marker::EpisodeEvidence],
    policy: &marker::SamplingPolicy,
) -> Vec<EpisodeFingerprintDecision> {
    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
    for decision in decisions {
        groups
            .entry((
                decision.ledger_id.clone(),
                intro_search::name(decision.kind),
            ))
            .or_default()
            .push(decision);
    }
    groups
        .into_values()
        .filter_map(|mut group| {
            group.sort_by(|a, b| {
                a.window
                    .start_ms
                    .cmp(&b.window.start_ms)
                    .then(a.sample_id.cmp(&b.sample_id))
            });
            let verified = group
                .iter()
                .filter_map(|d| match &d.decision {
                    FingerprintDecision::Verified(i) => Some((d, i)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let Some((winner, _)) = verified.first() {
                let mut result = (*winner).clone();
                let tolerance = policy.max_reference_boundary_delta_ms.max(0) as u64;
                if verified.iter().enumerate().any(|(index, (_, interval))| {
                    verified[index + 1..].iter().any(|(_, other)| {
                        interval.start_ms.abs_diff(other.start_ms) > tolerance
                            || interval.end_ms.abs_diff(other.end_ms) > tolerance
                    })
                }) {
                    result.decision =
                        FingerprintDecision::Uncertain("conflicting_verified_windows".into());
                }
                return Some(result);
            }
            let mut result = group.last()?.clone();
            if result.kind == SegmentKind::Intro {
                let reasons = group
                    .iter()
                    .filter_map(|d| match &d.decision {
                        FingerprintDecision::Uncertain(r) => Some(r.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                result.decision = FingerprintDecision::Uncertain(format!(
                    "sampling_limit: independent Intro window is not whole search absence; {}",
                    reasons.join("; ")
                ));
            } else if let Some(selected) = group
                .iter()
                .find(|d| {
                    matches!(&d.decision, FingerprintDecision::Uncertain(reason)
                    if reason.contains("conflicting") || reason.contains("ambiguous"))
                })
                .or_else(|| {
                    group
                        .iter()
                        .find(|d| full_tail_no_match(d, target_evidence, policy))
                })
                .or_else(|| {
                    group
                        .iter()
                        .find(|d| matches!(d.decision, FingerprintDecision::Uncertain(_)))
                })
            {
                result = selected.clone();
            }
            Some(result)
        })
        .collect()
}

pub(super) fn request_next(
    manager: &ProbeManager,
    claim: &store::ProbeAnalysisClaim,
    decisions: &mut [EpisodeFingerprintDecision],
    target_evidence: &[&marker::EpisodeEvidence],
    inputs: &Inputs,
    policy: &marker::SamplingPolicy,
    stable_intro: bool,
) -> Result<bool, store::StoreError> {
    let mut requested = false;
    // Validate every participant before any pending transition can launch IO.
    let plans = claim
        .participants
        .iter()
        .map(|participant| load_plan(manager, participant))
        .collect::<Result<Vec<_>, _>>()?;
    for (participant, loaded) in claim.participants.iter().zip(plans) {
        let Some((unit, mut plan)) = loaded else {
            continue;
        };
        let id = &participant.dispatch().ledger_id;
        let mut changed = false;
        let mut terminal = false;
        for decision in decisions.iter_mut().filter(|d| {
            &d.ledger_id == id && !matches!(d.decision, FingerprintDecision::Verified(_))
        }) {
            if full_tail_no_match(decision, target_evidence, policy) {
                continue;
            }
            let Some(search) = plan.searches.iter_mut().find(|s| s.kind == decision.kind) else {
                continue;
            };
            if search.pending.is_some() {
                continue;
            }
            if let Some(reason) = &search.terminal_reason {
                decision.decision = FingerprintDecision::Uncertain(reason.clone());
                continue;
            }
            let Some(evidence) = target_evidence
                .iter()
                .find(|e| e.sample_id == decision.sample_id)
            else {
                continue;
            };
            let next = match next_window(search, decision, evidence, inputs, policy, stable_intro) {
                NextCapture::Stop => continue,
                NextCapture::Window(window) => Some(window),
                NextCapture::Exhausted => None,
            };
            if let Some(window) = next {
                search.pending = Some(window);
                changed = true;
            } else {
                let reason = "sampling_limit: finite independent windows exhausted; seam context is not joined".to_string();
                search.terminal_reason = Some(reason.clone());
                decision.decision = FingerprintDecision::Uncertain(reason);
                terminal = true;
            }
        }
        if changed {
            if !manager.store.request_probe_analysis_capture(
                claim,
                participant.dispatch(),
                &serde_json::to_string(&plan)?,
            )? {
                return Err(store::StoreError::Protected(
                    "pending capture claim changed".into(),
                ));
            }
            requested = true;
        } else if terminal
            && !manager.store.put_probe_sampling_plan(
                &unit.job_id,
                id,
                &serde_json::to_string(&plan)?,
            )?
        {
            return Err(store::StoreError::Protected(
                "terminal capture plan changed".into(),
            ));
        }
    }
    Ok(requested)
}
fn load_plan(
    manager: &ProbeManager,
    participant: &store::ProbeAnalysisParticipant,
) -> Result<
    Option<(
        store::ProbeJobUnit,
        super::super::super::capture::CapturePlan,
    )>,
    store::StoreError,
> {
    let id = &participant.dispatch().ledger_id;
    let Some(unit) = manager.store.active_probe_unit_for_ledger(id)? else {
        return Ok(None);
    };
    if unit.job_id != participant.dispatch().job_id {
        return Err(store::StoreError::Protected(
            "capture plan ownership changed".into(),
        ));
    }
    let work = super::super::super::ProbeUnit {
        row: participant.context().target.clone(),
        kind: participant.context().target.file.kind,
        force_fingerprint: unit.force_fingerprint,
        reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
        overwrite_markers: unit.overwrite_markers,
        reuse_media_info_cache: unit.reuse_media_info_cache,
        marker_refresh_id: None,
        job_id: Some(unit.job_id.clone()),
    };
    let Some(plan) = super::super::super::capture::CapturePlan::consume(
        manager,
        &work,
        unit.sampling_plan_json.as_deref().unwrap_or(""),
        super::super::super::metadata_duration(manager, &work.row)?,
    )?
    else {
        return Err(store::StoreError::Protected(
            "invalid capture plan participant finished; claim must be renewed".into(),
        ));
    };
    Ok(Some((unit, plan)))
}

enum NextCapture {
    Stop,
    Window(marker::SampleWindow),
    Exhausted,
}
fn next_window(
    search: &KindSearch,
    decision: &EpisodeFingerprintDecision,
    evidence: &marker::EpisodeEvidence,
    inputs: &Inputs,
    policy: &marker::SamplingPolicy,
    stable_intro: bool,
) -> NextCapture {
    if !intro_search::eligible(evidence) {
        return NextCapture::Stop;
    }
    if let FingerprintDecision::Uncertain(reason) = &decision.decision
        && (reason.contains("conflicting")
            || reason.contains("ambiguous")
            || reason.contains("unusable_reference"))
    {
        return NextCapture::Stop;
    }
    let next = if decision.kind == SegmentKind::Intro {
        let independent = inputs
            .evidence
            .iter()
            .filter(|e| {
                e.kind == SegmentKind::Intro
                    && intro_search::eligible(e)
                    && super::model_groups::geometry(e) == super::model_groups::geometry(evidence)
            })
            .map(|e| e.episode)
            .collect::<HashSet<_>>()
            .len();
        if independent < policy.seed_count && !stable_intro {
            return NextCapture::Stop;
        }
        search.next(evidence.duration_ms, policy.max_windows_per_kind)
    } else {
        outro_fallback(search, evidence, policy)
    };
    match next {
        Some(window) => NextCapture::Window(window),
        None => NextCapture::Exhausted,
    }
}

fn outro_fallback(
    search: &KindSearch,
    evidence: &marker::EpisodeEvidence,
    policy: &marker::SamplingPolicy,
) -> Option<marker::SampleWindow> {
    let duration = evidence.duration_ms?;
    let descriptor = marker::EpisodeDescriptor {
        ledger_id: evidence.ledger_id.clone(),
        episode: evidence.episode,
        source_version: evidence.source_version.clone(),
        duration_ms: duration,
    };
    let decision = marker::adaptive::plan_episode_window(
        &descriptor,
        SegmentKind::Outro,
        &marker::TemplateContext::default(),
        policy,
        &Default::default(),
    );
    let window = match decision {
        marker::WindowDecision::Full { window, .. }
        | marker::WindowDecision::Verify { window, .. } => window,
    };
    (search.windows.len() < policy.max_windows_per_kind && !search.windows.contains(&window))
        .then_some(window)
}

// Only native strict absence in the actual default tail may replace a narrow
// inconclusive result. The output merger still validates the old marker's coverage.
fn full_tail_no_match(
    decision: &EpisodeFingerprintDecision,
    target_evidence: &[&marker::EpisodeEvidence],
    policy: &marker::SamplingPolicy,
) -> bool {
    if decision.kind != SegmentKind::Outro
        || !matches!(decision.decision, FingerprintDecision::NoMatch)
    {
        return false;
    }
    target_evidence.iter().any(|evidence| {
        evidence.sample_id == decision.sample_id
            && evidence.ledger_id == decision.ledger_id
            && evidence.kind == SegmentKind::Outro
            && evidence.duration_ms.is_some_and(|duration| {
                let descriptor = marker::EpisodeDescriptor {
                    ledger_id: evidence.ledger_id.clone(),
                    episode: evidence.episode,
                    source_version: evidence.source_version.clone(),
                    duration_ms: duration,
                };
                duration > 0
                    && decision.window == evidence.capture.window
                    && decision.window
                        == marker::adaptive::planner::default_full_window(
                            &descriptor,
                            SegmentKind::Outro,
                            policy,
                        )
                    && marker::boundary::evidence::complete_pcm(&evidence.capture)
            })
    })
}
