use super::{lifecycle::MediaWork, *};
use serde_json::json;
use std::collections::BTreeSet;

pub(super) fn assemble(
    session: BenchmarkSession,
    work: Vec<MediaWork>,
    elapsed_ms: u64,
    expired: bool,
) -> Result<BenchmarkRun, String> {
    let JobObservations {
        jobs,
        summaries,
        attempts,
        queue_wait,
    } = collect_jobs(&session, &work)?;
    let (frame_observations, dropped) = session.state.chapter_frames().measurement_snapshot();
    let frames = collect_frames(&session, &frame_observations)?;
    let complete = !expired
        && !work.is_empty()
        && session.inputs.iter().all(|input| input.error.is_none())
        && work.iter().all(|media| {
            media
                .terminal
                .as_ref()
                .is_some_and(|m| m.wallclock_complete)
        });
    let predictions = session
        .inputs
        .iter()
        .map(|input| prediction(&session, &work, input))
        .collect::<Result<Vec<_>, _>>()?;
    let complete = complete && predictions.iter().all(|p| p.state != "error");
    let media = work
        .iter()
        .filter_map(|work| work.terminal.clone())
        .map(|mut media| {
            if media.terminal_status == TerminalStatus::Completed
                && session
                    .inputs
                    .iter()
                    .filter(|input| input.case.media_group == media.media_group)
                    .any(|input| {
                        predictions
                            .iter()
                            .any(|p| p.case_id == input.case.case_id && p.state == "error")
                    })
            {
                media.wallclock_complete = false;
                media.terminal_status = TerminalStatus::Failed;
            }
            media
        })
        .collect::<Vec<_>>();
    let source_verification = session.inputs.iter().map(|input| {
        let registered = session.registered.iter().find(|s| s.original_path == input.source.path);
        json!({"case_id":input.case.case_id,"source_hash":input.case.source_hash,"status":if input.error.is_some(){"error"}else{"verified_whole_local_file"},
            "reason":input.error,"file_id":registered.map(|s| s.row.file.file_id),"unit":input.source.unit,"audio_stream_index":input.source.audio_stream_index})
    }).collect();
    let stage_totals = BenchmarkStages {
        queue_wait,
        remote_gate_wait: sum_summaries(&summaries, |s| s.stage_totals.remote_gate_wait),
        capture: sum_summaries(&summaries, |s| s.stage_totals.capture),
        compare: sum_summaries(&summaries, |s| s.stage_totals.compare),
        refine: sum_summaries(&summaries, |s| s.stage_totals.refine),
        frame: if dropped == 0 {
            frame_observations
                .iter()
                .try_fold(0u64, |sum, row| row.frame_ms.map(|ms| sum + ms))
        } else {
            None
        },
        // Frame publication observations are in frames; marker publication lacks a complete span.
        publish: None,
    };
    let run = BenchmarkRun {
        schema_version:1, run_id:session.options.run_id.clone(), pair_id:session.options.pair_id.clone(), variant:session.options.variant.clone(), scenario:session.options.scenario.clone(),
        corpus_version:session.manifest.corpus_version.clone(), config_hash:provenance::config_hash(&session.options, &session.build)?,
        source_hashes:session.manifest.cases.iter().map(|case| case.source_hash.clone()).collect::<BTreeSet<_>>().into_iter().collect(),
        commit:env!("GIT_HASH").into(), mode:session.options.mode.clone(), capture_policy:session.options.capture_policy, build:session.build,
        total_elapsed_ms:elapsed_ms, wallclock_complete:complete, terminal_status:if expired {TerminalStatus::Incomplete} else if complete {TerminalStatus::Completed} else {TerminalStatus::Failed},
        stage_totals, input_bytes:None, measurement_complete:false,
        raw_pairs_computed:sum_summaries(&summaries, |s| s.raw_pairs_computed), raw_pairs_reused:sum_summaries(&summaries, |s| s.raw_pairs_reused),
        attempts, predictions, jobs, frames, source_verification, media,
        limitations:vec!["Current-version absolute lifecycle time; no baseline speedup claim.".into(),
            "Source hashing/import/build occur before the media-job clock; cold means application cache, not cold OS cache.".into(),
            "Operation-stage sums may overlap across workers; never sum them into wallclock. Whole-run input traffic and complete marker-publication timing are unavailable.".into(),
            "STRM text and saved PCM do not verify the complete remote source; remote sources remain unverified without complete-content evidence.".into()],
    };
    std::fs::create_dir_all(&session.options.output).map_err(|e| e.to_string())?;
    input::write_json(
        &session.options.output.join(format!("{}.json", run.run_id)),
        &run,
    )?;
    input::write_json(
        &session
            .options
            .output
            .join(format!("{}-predictions.json", run.run_id)),
        &run.predictions,
    )?;
    Ok(run)
}

struct JobObservations {
    jobs: Vec<serde_json::Value>,
    summaries: Vec<Option<crate::probe_manager::timings::ProbeTimingSummary>>,
    attempts: Vec<BenchmarkAttempt>,
    queue_wait: Option<u64>,
}

fn collect_jobs(session: &BenchmarkSession, work: &[MediaWork]) -> Result<JobObservations, String> {
    let mut jobs = Vec::new();
    let mut summaries = Vec::new();
    let mut attempts = Vec::new();
    let mut queue_wait = Some(0u64);
    let ids = work
        .iter()
        .flat_map(|media| media.jobs.iter().cloned())
        .collect::<BTreeSet<_>>();
    for id in &ids {
        let job = session
            .state
            .store
            .get_probe_job(id)
            .map_err(|e| e.to_string())?
            .ok_or("evaluation job disappeared")?;
        queue_wait = queue_wait
            .zip(job.started_at_ms)
            .map(|(sum, start)| sum + start.saturating_sub(job.created_at_ms).max(0) as u64);
        let units = session
            .state
            .store
            .probe_job_units(id)
            .map_err(|e| e.to_string())?;
        jobs.push(json!({"id":job.id,"kind":job.kind,"media_id":job.media_id,"season":job.season,
            "status":job.status,"total":job.total,"completed":job.completed,"succeeded":job.succeeded,
            "failed":job.failed,"partial":job.partial,"error":job.error,"created_at_ms":job.created_at_ms,
            "started_at_ms":job.started_at_ms,"finished_at_ms":job.finished_at_ms,
            "units":units.iter().map(|unit| json!({"ledger_id":unit.ledger_id,"status":unit.status,
                "error":unit.error,"detection_outcome_json":unit.detection_outcome_json,
                "force_fingerprint":unit.force_fingerprint,"reuse_fingerprint_cache":unit.reuse_fingerprint_cache})).collect::<Vec<_>>()}));
        if let Some(summary) = session.state.probe.timing_snapshot(id) {
            attempts.extend(summary.capture_attempts.clone());
            summaries.push(Some(summary));
        } else {
            summaries.push(None);
        }
    }
    Ok(JobObservations {
        jobs,
        summaries,
        attempts,
        queue_wait,
    })
}

fn collect_frames(
    session: &BenchmarkSession,
    frame_observations: &[crate::chapter_frame_service::ChapterFrameMeasurement],
) -> Result<Vec<serde_json::Value>, String> {
    let mut frames = Vec::new();
    for source in &session.registered {
        let task = session
            .state
            .store
            .current_chapter_frame_task(source.row.file.file_id)
            .map_err(|e| e.to_string())?;
        frames.push(json!({"file_id":source.row.file.file_id, "status":task.as_ref().map(|task| &task.status),
            "targets":task.as_ref().map(|task| &task.targets), "measurements":frame_observations.iter().filter(|m| m.file_id == source.row.file.file_id).collect::<Vec<_>>()}));
    }
    Ok(frames)
}

fn sum_summaries(
    summaries: &[Option<crate::probe_manager::timings::ProbeTimingSummary>],
    get: impl Fn(&crate::probe_manager::timings::ProbeTimingSummary) -> Option<u64>,
) -> Option<u64> {
    if summaries.is_empty() {
        return None;
    }
    summaries.iter().try_fold(0u64, |sum, s| {
        get(s.as_ref()?).map(|value| sum.saturating_add(value))
    })
}

fn prediction(
    session: &BenchmarkSession,
    work: &[MediaWork],
    input: &input::VerifiedInput,
) -> Result<BenchmarkPrediction, String> {
    let mut prediction = BenchmarkPrediction {
        case_id: input.case.case_id.clone(),
        source_hash: input.case.source_hash.clone(),
        state: "error".into(),
        interval_ms: None,
        reason: input
            .error
            .clone()
            .unwrap_or_else(|| "source registration unavailable".into()),
        run_id: session.options.run_id.clone(),
    };
    if input.error.is_some() {
        return Ok(prediction);
    }
    let Some(source) = session
        .registered
        .iter()
        .find(|s| s.original_path == input.source.path)
    else {
        return Ok(prediction);
    };
    let media = work
        .iter()
        .find(|m| m.group == input.case.media_group)
        .ok_or("Media observation missing")?;
    if !media
        .terminal
        .as_ref()
        .is_some_and(|m| m.wallclock_complete)
    {
        prediction.reason = media
            .error
            .clone()
            .unwrap_or_else(|| "production_lifecycle_failed_or_incomplete".into());
        return Ok(prediction);
    }
    let tracks = session
        .state
        .store
        .get_file_meta(&source.row.file.file_id.to_string())
        .map_err(|e| e.to_string())?;
    if tracks.as_ref().is_none_or(|tracks| {
        crate::fingerprint_job::capture_source(tracks, Some(input.source.audio_stream_index))
            .is_err()
    }) {
        prediction.reason = "requested_audio_stream_unverified".into();
        return Ok(prediction);
    }
    'latest_outcome: for id in media.jobs.iter().rev() {
        for unit in session
            .state
            .store
            .probe_job_units(id)
            .map_err(|e| e.to_string())?
        {
            if unit.ledger_id == source.row.file.file_id.to_string()
                && let Some(outcome) = unit
                    .detection_outcome_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            {
                let kind = &outcome["kinds"][&input.case.kind];
                if outcome["status"] == "uncertain"
                    || kind["status"] == "uncertain"
                    || kind["retained_previous"] == true
                {
                    prediction.state = "uncertain".into();
                    prediction.reason = kind["reason"]
                        .as_str()
                        .unwrap_or("verification_uncertain")
                        .into();
                    return Ok(prediction);
                }
                break 'latest_outcome;
            }
        }
    }
    let marker = session
        .state
        .store
        .get_file_marker(source.row.file.file_id, source.row.units[0])
        .map_err(|e| e.to_string())?;
    let interval = marker.and_then(|marker| {
        if input.case.kind == "intro" {
            marker.intro_start_ms.zip(marker.intro_end_ms)
        } else {
            marker.outro_start_ms.zip(marker.outro_end_ms)
        }
    });
    match interval {
        Some((start, end)) if start >= 0 && start < end && end <= input.case.duration_ms => {
            prediction.state = "matched".into();
            prediction.interval_ms = Some([start, end]);
            prediction.reason = "production_verified_marker".into();
        }
        Some(_) => prediction.reason = "published_interval_outside_manifest_duration".into(),
        None => {
            prediction.state = "uncertain".into();
            prediction.reason = "no_published_interval_without_persisted_typed_absence".into();
        }
    }
    Ok(prediction)
}
