use super::super::stages::FingerprintRunOutcome;
use super::*;
use crate::fingerprint_job::{AdaptiveCaptureContext, CapturePolicy, EpisodeCaptureRequest};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(in crate::probe_manager) struct CapturePlan {
    pub searches: Vec<super::intro_search::KindSearch>,
    pub selected_samples: Vec<String>,
    pub policy: marker::SamplingPolicy,
    pub sampling_mode: String,
}

pub(in crate::probe_manager) fn prepare_plan(
    manager: &ProbeManager,
    work: &super::super::probe::FingerprintWork,
) -> Result<CapturePlan, store::StoreError> {
    let unit = &work.unit;
    if let Some(id) = &unit.job_id
        && let Some(plan) = manager
            .store
            .active_probe_unit_for_ledger(&unit.row.file.file_id.to_string())?
            .filter(|u| u.job_id == *id)
            .and_then(|u| u.sampling_plan_json)
    {
        return CapturePlan::parse(&plan, work.media_duration_ms);
    }
    let config = manager.store.get_scrape_config()?;
    let plan = CapturePlan {
        searches: vec![],
        selected_samples: vec![],
        policy: marker::SamplingPolicy {
            full_window_duration_secs: work.duration_secs,
            max_windows_per_kind: if config.effective.fingerprint_sampling_mode == "adaptive" {
                3
            } else {
                2
            },
            ..Default::default()
        },
        sampling_mode: config.effective.fingerprint_sampling_mode,
    };
    plan.validate(work.media_duration_ms)?;
    if let Some(id) = &unit.job_id {
        manager
            .store
            .pin_probe_job_sampling_plan(id, &serde_json::to_string(&plan)?)?;
    }
    Ok(plan)
}

pub(in crate::probe_manager) async fn capture_target(
    manager: &ProbeManager,
    unit: &ProbeUnit,
    plan: &CapturePlan,
) -> FingerprintRunOutcome {
    match capture(manager, unit, plan).await {
        Ok(()) => FingerprintRunOutcome::Complete,
        Err(error) if error.contains("file_deleted") => {
            FingerprintRunOutcome::Cancelled { reason: error }
        }
        Err(error) => FingerprintRunOutcome::Partial {
            error_kind: if error.contains("budget exhausted")
                || error.starts_with("sampling_limit:")
            {
                "sampling_limit"
            } else {
                "network"
            }
            .into(),
            error,
        },
    }
}

async fn capture(
    manager: &ProbeManager,
    unit: &ProbeUnit,
    plan: &CapturePlan,
) -> Result<(), String> {
    let job_id = unit.job_id.clone().ok_or("capture job missing")?;
    let policy = plan.policy.clone();
    let season = super::super::require_episode_coordinates(&unit.row)
        .map_err(|e| e.to_string())?
        .0;
    let templates = models::load(
        manager,
        SeasonKey {
            media_id: unit.row.file.media_id,
            season,
        },
        &policy,
        None,
    )
    .map_err(|e| e.to_string())?;
    let duration = metadata_duration(manager, &unit.row).map_err(|e| e.to_string())?;
    let source = manager.capture_source(&unit.row.file);
    let (gate, _leases) = super::super::progress::MetadataPriorityGate::for_rows(
        manager,
        std::slice::from_ref(unit),
    )?;
    let context = AdaptiveCaptureContext {
        store: manager.store_access(),
        matcher: manager.fingerprint_engine.clone(),
        capture: Arc::new(
            crate::fingerprint_job::capture_measurement::CaptureMeasurement {
                engine: manager.capture_engine.clone(),
                timings: manager.timings.clone(),
                job_id: Some(job_id.clone()),
            },
        ),
        gate: Arc::new(gate),
        timings: Some(manager.timings.clone()),
    };
    let request = EpisodeCaptureRequest {
        job_id: job_id.clone(),
        row: unit.row.clone(),
        source_version: unit.row.file.version.source_version.clone(),
        media_duration_ms: duration,
        audio_stream_index: source.as_ref().map(|s| s.audio_stream_index),
        capture_profile_key: crate::fingerprint_job::capture_profile_key(
            &crate::fingerprint_job::profile_for_source(source.as_ref()),
        ),
        policy: policy.clone(),
        capture_policy: if unit.reuse_fingerprint_cache {
            CapturePolicy::ReuseValid
        } else {
            CapturePolicy::Recapture
        },
        templates,
        cost_summary: Default::default(),
    };
    let stage_key = crate::fingerprint_job::key_for_file(
        &manager.store,
        &unit.row.file,
        policy.full_window_duration_secs,
        duration,
    );
    capture_pending(manager, unit, plan, &context, &request, &stage_key).await
}

async fn capture_pending(
    manager: &ProbeManager,
    unit: &ProbeUnit,
    plan: &CapturePlan,
    context: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    stage_key: &str,
) -> Result<(), String> {
    let job_id = &request.job_id;
    let policy = &request.policy;
    let mut captured_plan = plan.clone();
    initialize_searches(unit, request, &mut captured_plan)?;
    for index in 0..captured_plan.searches.len() {
        let Some(window) = captured_plan.searches[index].pending.clone() else {
            continue;
        };
        let kind = captured_plan.searches[index].kind;
        let receipts = manager
            .store
            .list_fingerprint_attempts_for_job(&job_id)
            .map_err(|e| e.to_string())?;
        let search = &mut captured_plan.searches[index];
        for receipt in receipts.iter().filter(|a| {
            a.ledger_id == unit.row.file.file_id.to_string()
                && a.kind == super::intro_search::name(kind)
        }) {
            search.record(marker::SampleWindow::new(
                receipt.window_start_ms,
                receipt.window_end_ms,
            ));
        }
        if !search.windows.contains(&window) && search.windows.len() >= policy.max_windows_per_kind
        {
            search.pending = None;
            search.terminal_reason =
                Some("sampling_limit: distinct window budget exhausted".into());
            persist(manager, unit, &captured_plan)?;
            continue;
        }
        let reserved = !search.windows.contains(&window);
        search.record(window.clone());
        persist(manager, unit, &captured_plan)?;
        let sample =
            match capture_kind(manager, &context, &request, kind, &window, &stage_key).await {
                Ok(sample) => sample,
                Err(error)
                    if error.contains("budget exhausted")
                        && !captured_plan.selected_samples.is_empty() =>
                {
                    let search = &mut captured_plan.searches[index];
                    search.pending = None;
                    search.terminal_reason = Some(format!("sampling_limit: {error}"));
                    persist(manager, unit, &captured_plan)?;
                    continue;
                }
                Err(error) => return Err(error),
            };
        let search = &mut captured_plan.searches[index];
        // A cache hit has its own actual geometry. A request that did no physical
        // work does not consume an additional off-book or duplicate window slot.
        if reserved && sample.capture.window != window {
            search.windows.retain(|old| old != &window);
        }
        if !search.windows.contains(&sample.capture.window)
            && search.windows.len() >= policy.max_windows_per_kind
        {
            search.pending = None;
            search.terminal_reason =
                Some("sampling_limit: actual cached geometry exceeds window budget".into());
            persist(manager, unit, &captured_plan)?;
            continue;
        }
        search.record(sample.capture.window.clone());
        search.pending = None;
        if !captured_plan.selected_samples.contains(&sample.sample_id) {
            captured_plan.selected_samples.push(sample.sample_id);
        }
        persist(manager, unit, &captured_plan)?;
    }
    Ok(())
}

fn initialize_searches(
    unit: &ProbeUnit,
    request: &EpisodeCaptureRequest,
    plan: &mut CapturePlan,
) -> Result<(), String> {
    let duration = request.media_duration_ms;
    let policy = &request.policy;
    if plan.searches.is_empty() {
        for kind in [marker::SegmentKind::Intro, marker::SegmentKind::Outro] {
            if kind == marker::SegmentKind::Outro && duration.is_none() {
                continue;
            }
            let descriptor = marker::adaptive::EpisodeDescriptor {
                ledger_id: unit.row.file.file_id.to_string(),
                episode: super::super::require_episode_coordinates(&unit.row)
                    .map_err(|e| e.to_string())?
                    .1,
                source_version: request.source_version.clone(),
                duration_ms: duration.unwrap_or(0),
            };
            let decision = marker::adaptive::plan_episode_window(
                &descriptor,
                kind,
                &request.templates,
                &policy,
                &request.cost_summary,
            );
            let window = match decision {
                marker::adaptive::WindowDecision::Full { window, .. }
                | marker::adaptive::WindowDecision::Verify { window, .. } => window,
            };
            plan.searches
                .push(super::intro_search::KindSearch::new(kind, window));
        }
    }
    Ok(())
}

fn persist(manager: &ProbeManager, unit: &ProbeUnit, plan: &CapturePlan) -> Result<(), String> {
    let job = unit.job_id.as_deref().ok_or("capture job missing")?;
    if !manager
        .store
        .put_probe_sampling_plan(
            job,
            &unit.row.file.file_id.to_string(),
            &serde_json::to_string(plan).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
    {
        return Err("capture plan ownership lost".into());
    }
    Ok(())
}

async fn capture_kind(
    manager: &ProbeManager,
    context: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    kind: marker::SegmentKind,
    window: &marker::SampleWindow,
    stage_key: &str,
) -> Result<marker::EpisodeEvidence, String> {
    let job_id = &request.job_id;
    let duration = request.media_duration_ms;
    let descriptor = marker::adaptive::EpisodeDescriptor {
        ledger_id: request.row.file.file_id.to_string(),
        episode: super::super::require_episode_coordinates(&request.row)
            .map_err(|e| e.to_string())?
            .1,
        source_version: request.source_version.clone(),
        duration_ms: duration.unwrap_or(0),
    };
    let key = store::ProbeStageKey {
        ledger_id: descriptor.ledger_id.clone(),
        context_key: stage_key.to_string(),
        stage: if kind == marker::SegmentKind::Intro {
            store::ProbeStage::Intro
        } else {
            store::ProbeStage::Outro
        },
    };
    let now = manager.clock.lock().now_ms();
    manager
        .store
        .library_write(|db| {
            db.adopt_probe_context(&descriptor.ledger_id, &stage_key)?;
            if !db.claim_admitted_capture_stage(&key, &job_id, now)? {
                return Err(store::StoreError::Protected(
                    "capture stage ownership lost".into(),
                ));
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    let mut attempts = manager
        .store
        .list_fingerprint_attempts_for_job(job_id)
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|a| {
            a.ledger_id == descriptor.ledger_id && a.kind == super::intro_search::name(kind)
        })
        .count();
    let result = crate::fingerprint_job::adaptive::capture::capture_bounded_sample(
        &context,
        &request,
        kind,
        &window,
        &mut attempts,
    )
    .await;
    let completion = match &result {
        Ok(_) => store::ProbeStageCompletion::Succeeded,
        Err(error) if error.contains("budget exhausted") => {
            store::ProbeStageCompletion::NotApplicable
        }
        Err(error) => store::ProbeStageCompletion::Failed {
            error_kind: "network".into(),
            error: error.clone(),
        },
    };
    let now = manager.clock.lock().now_ms();
    let finished = manager
        .store
        .finish_probe_stage(&key, &job_id, &completion, now)
        .map_err(|e| e.to_string())?;
    if !finished {
        return Err("capture stage ownership lost".into());
    }
    result
}
