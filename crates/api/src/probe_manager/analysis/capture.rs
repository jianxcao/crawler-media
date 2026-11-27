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
    let adaptive = config.effective.fingerprint_sampling_mode == "adaptive";
    let default_policy = marker::SamplingPolicy::default();
    let plan = CapturePlan {
        searches: vec![],
        selected_samples: vec![],
        policy: marker::SamplingPolicy {
            full_window_duration_secs: if adaptive {
                default_policy.full_window_duration_secs
            } else {
                work.duration_secs
            },
            max_windows_per_kind: if adaptive { 3 } else { 2 },
            ..default_policy
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
    control: &marker::WorkControl,
) -> FingerprintRunOutcome {
    match capture(manager, unit, plan, control).await {
        Ok(()) => FingerprintRunOutcome::Complete,
        Err(error) if error == marker::media_read::READ_WAIT_STOPPED => {
            FingerprintRunOutcome::Retry
        }
        Err(error)
            if error.contains("file_deleted")
                || error.contains("Library work cancelled")
                || error.contains("Cancelled:") =>
        {
            FingerprintRunOutcome::Cancelled { reason: error }
        }
        Err(error) if error.contains("unit_time_mapping_unsupported") => {
            FingerprintRunOutcome::Failed {
                error_kind: "unit_time_mapping_unsupported".into(),
                error,
            }
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
    control: &marker::WorkControl,
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
    let gate = gate.with_unit_control(unit.row.file.file_id.to_string(), control.clone());
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
        cost_summary: load_source_cost_summary(manager, unit.row.file.media_id, season),
    };
    let stage_key = crate::fingerprint_job::key_for_file(
        &manager.store,
        &unit.row.file,
        policy.full_window_duration_secs,
        duration,
    );
    capture_pending(manager, unit, plan, &context, &request, &stage_key).await
}

fn load_source_cost_summary(
    manager: &ProbeManager,
    media_id: domain::MediaId,
    season: u32,
) -> marker::adaptive::SourceCostSummary {
    let Ok(attempts) = manager
        .store
        .list_season_fingerprint_attempts(media_id, season, 48)
    else {
        return Default::default();
    };
    observations_to_cost_summary(&attempts)
}

fn observations_to_cost_summary(
    attempts: &[store::StoredFingerprintAttempt],
) -> marker::adaptive::SourceCostSummary {
    let mut group_order: Vec<(String, String, String)> = Vec::new();
    let mut groups: std::collections::HashMap<
        (String, String, String),
        Vec<&store::StoredFingerprintAttempt>,
    > = std::collections::HashMap::new();
    for attempt in attempts {
        let key = (
            attempt.job_id.clone(),
            attempt.ledger_id.clone(),
            attempt.kind.clone(),
        );
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
        }
        groups.entry(key).or_default().push(attempt);
    }
    let mut observations = Vec::with_capacity(attempts.len());
    for key in group_order {
        let Some(group) = groups.get(&key) else {
            continue;
        };
        let mut saw_fast_window: Option<(i64, i64)> = None;
        for attempt in group {
            let window_duration_ms = (attempt.window_end_ms - attempt.window_start_ms).max(0);
            let parsed = serde_json::from_str::<marker::CaptureMetrics>(&attempt.metrics_json).ok();
            let fallback_elapsed = attempt
                .finished_at_ms
                .map(|fin| fin.saturating_sub(attempt.started_at_ms).max(0) as u64)
                .unwrap_or(0);
            let succeeded = attempt.status == "succeeded";
            let is_scheduled_fast = attempt.phase == "fast_verify"
                || (attempt.phase == "scheduled_capture"
                    && window_duration_ms > 0
                    && window_duration_ms < 160_000);
            let is_followup_after_fast = saw_fast_window.is_some_and(|first_win| {
                attempt.phase == "scheduled_capture"
                    && (attempt.window_start_ms, attempt.window_end_ms) != first_win
            });
            let phase = if is_followup_after_fast {
                "fallback_full_window".to_string()
            } else {
                if is_scheduled_fast && saw_fast_window.is_none() {
                    saw_fast_window = Some((attempt.window_start_ms, attempt.window_end_ms));
                }
                attempt.phase.clone()
            };
            if succeeded {
                let Some(metrics) =
                    parsed.filter(|m| m.measurement_complete && m.time_to_first_pcm_ms.is_some())
                else {
                    continue;
                };
                let raw_elapsed = if metrics.elapsed_ms > 0 {
                    metrics.elapsed_ms
                } else {
                    fallback_elapsed
                };
                observations.push(marker::adaptive::AttemptCostObservation {
                    phase,
                    succeeded: true,
                    window_duration_ms,
                    elapsed_ms: raw_elapsed,
                    time_to_first_pcm_ms: metrics.time_to_first_pcm_ms,
                });
            } else if attempt.status == "failed" {
                let raw_elapsed = parsed
                    .as_ref()
                    .map(|m| m.elapsed_ms)
                    .filter(|&e| e > 0)
                    .unwrap_or(fallback_elapsed);
                observations.push(marker::adaptive::AttemptCostObservation {
                    phase,
                    succeeded: false,
                    window_duration_ms,
                    elapsed_ms: raw_elapsed,
                    time_to_first_pcm_ms: None,
                });
            }
        }
    }
    marker::adaptive::SourceCostSummary::from_observations(&observations)
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
    if matches!(&result, Err(error) if error == marker::media_read::READ_WAIT_STOPPED) {
        return result;
    }
    let completion = match &result {
        Ok(_) => store::ProbeStageCompletion::Succeeded,
        Err(error) if error.contains("budget exhausted") => {
            store::ProbeStageCompletion::NotApplicable
        }
        Err(error)
            if crate::fingerprint_job::source::is_file_deleted(error)
                || error.contains("Library work cancelled")
                || error.contains("Cancelled:") =>
        {
            store::ProbeStageCompletion::Cancelled {
                reason: error.clone(),
            }
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
    if !finished && result.is_ok() {
        return Err("capture stage ownership lost".into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::observations_to_cost_summary;

    #[test]
    fn cost_summary_groups_by_job_ledger_kind_subtracts_first_pcm_and_tracks_fallbacks() {
        let mk_metrics = |elapsed: u64, first_pcm: Option<u64>, complete: bool| {
            serde_json::to_string(&marker::CaptureMetrics {
                elapsed_ms: elapsed,
                time_to_first_pcm_ms: first_pcm,
                measurement_complete: complete,
                ..Default::default()
            })
            .unwrap()
        };
        // Interleave attempts across episodes so naive chronological order would break pairing!
        let attempts = vec![
            store::StoredFingerprintAttempt {
                attempt_id: "a1".into(),
                job_id: "j1".into(),
                ledger_id: "ep1".into(),
                kind: "intro".into(),
                window_start_ms: 10_000,
                window_end_ms: 110_000,
                phase: "scheduled_capture".into(),
                started_at_ms: 100,
                finished_at_ms: Some(2_600),
                status: "succeeded".into(),
                error_kind: None,
                metrics_json: mk_metrics(2_500, Some(500), true),
            },
            store::StoredFingerprintAttempt {
                attempt_id: "a2".into(),
                job_id: "j1".into(),
                ledger_id: "ep2".into(),
                kind: "intro".into(),
                window_start_ms: 10_000,
                window_end_ms: 110_000,
                phase: "scheduled_capture".into(),
                started_at_ms: 200,
                finished_at_ms: Some(1_200),
                status: "failed".into(),
                error_kind: Some("timeout".into()),
                metrics_json: mk_metrics(1_000, None, false),
            },
            store::StoredFingerprintAttempt {
                attempt_id: "a3".into(),
                job_id: "j1".into(),
                ledger_id: "ep1".into(),
                kind: "intro".into(),
                window_start_ms: 0,
                window_end_ms: 180_000,
                phase: "scheduled_capture".into(),
                started_at_ms: 3_000,
                finished_at_ms: Some(7_100),
                status: "succeeded".into(),
                error_kind: None,
                metrics_json: mk_metrics(4_100, Some(500), true),
            },
            store::StoredFingerprintAttempt {
                attempt_id: "a4".into(),
                job_id: "j1".into(),
                ledger_id: "ep3".into(),
                kind: "intro".into(),
                window_start_ms: 10_000,
                window_end_ms: 110_000,
                phase: "scheduled_capture".into(),
                started_at_ms: 4_000,
                finished_at_ms: Some(6_500),
                status: "succeeded".into(),
                error_kind: None,
                metrics_json: mk_metrics(2_500, Some(500), true),
            },
        ];

        let summary = observations_to_cost_summary(&attempts);
        assert_eq!(summary.measured_samples, 3);
        assert_eq!(summary.median_first_pcm_ms, Some(500));
        // Decode elapsed after subtracting 500ms first_pcm:
        // a1: 100_000 / 2_000 = 50.0, a3: 180_000 / 3_600 = 50.0, a4: 100_000 / 2_000 = 50.0
        let rate = summary.sample_ms_per_wall_ms.expect("rate");
        assert!((49.9..=50.1).contains(&rate), "expected 50.0, got {rate}");
        assert_eq!(summary.recent_fast_attempts, 3);
        assert_eq!(summary.recent_fallbacks, 2);
    }

    #[test]
    fn clean_eof_tail_capture_admits_sub_500ms_shortfall_only_at_media_end() {
        let mut capture = marker::CapturedFingerprint {
            window: marker::SampleWindow::new(1_320_000, 1_500_000),
            words: vec![1; 400],
            pcm_duration_ms: Some(179_680), // 320ms shortfall (< 500ms)
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                pcm_frame_ms: 20,
                ffmpeg_exit_code: Some(0),
                audio_stream_index: Some(0),
                source_facts: Some(marker::CaptureSourceFacts {
                    audio_stream_index: 0,
                    format_start_time_us: Some(0),
                    stream_start_pts: Some(0),
                    stream_time_base: Some("1/16000".into()),
                    codec_name: Some("aac".into()),
                    native_sample_rate: Some(48_000),
                    audio_channels: Some(2),
                    provenance: "fixture".into(),
                }),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(1_320_000),
                ..Default::default()
            },
        };
        assert!(
            super::super::intro_search::complete_or_clean_eof_pcm(&capture, Some(1_500_000)),
            "clean EOF tail with 320ms shortfall must be admitted"
        );
        assert!(
            !super::super::intro_search::complete_or_clean_eof_pcm(&capture, Some(1_600_000)),
            "interior window short by 320ms must stay incomplete"
        );
        capture.metrics.ffmpeg_exit_code = Some(1);
        assert!(
            !super::super::intro_search::complete_or_clean_eof_pcm(&capture, Some(1_500_000)),
            "non-zero ffmpeg exit code must never qualify as clean EOF"
        );
    }
}
