use super::{registration::RegisteredSource, *};
use crate::probe_manager::{
    ProbeUnit,
    policy::{ProbeRequestOrigin, ProbeRequestResult},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub(super) struct MediaWork {
    pub group: String,
    pub sources: Vec<RegisteredSource>,
    pub jobs: Vec<String>,
    pub started: Instant,
    pub terminal: Option<MediaMeasurement>,
    pub error: Option<String>,
}

pub(super) async fn execute(session: BenchmarkSession) -> Result<BenchmarkRun, String> {
    for source in &session.registered {
        session
            .state
            .probe
            .select_capture_audio(source.row.file.path.clone(), source.audio_stream_index);
    }
    let started = Instant::now();
    session.state.probe.try_start_workers();
    session.state.chapter_frames().start();
    let session = std::sync::Arc::new(session);
    let enqueue_session = session.clone();
    let mut work = tokio::task::spawn_blocking(move || enqueue(&enqueue_session))
        .await
        .map_err(|e| e.to_string())??;
    let mut expired = false;
    while work.iter().any(|media| media.terminal.is_none()) {
        let state = session.state.clone();
        let observations = work
            .iter()
            .map(|m| (m.sources.clone(), m.jobs.clone(), m.error.clone()))
            .collect::<Vec<_>>();
        let states = tokio::task::spawn_blocking(move || {
            observations
                .iter()
                .map(|(sources, jobs, error)| {
                    let jobs = associated_jobs(&state, sources, jobs)?;
                    let observation = inspect(&state, sources, &jobs, error.as_deref())?;
                    Ok::<_, String>((jobs, observation))
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| e.to_string())?;
        for (media, observation) in work.iter_mut().zip(states) {
            if media.terminal.is_some() {
                continue;
            }
            let (jobs, observation) = observation?;
            media.jobs = jobs;
            if let Some((status, targets, ready)) = observation {
                media.terminal = Some(measurement(media, status, targets, ready));
            }
        }
        if started.elapsed() >= Duration::from_millis(session.options.deadline_ms) {
            expired = true;
            break;
        }
        if work.iter().any(|media| media.terminal.is_none()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    record_incomplete(session.state.clone(), &mut work).await?;
    let elapsed = started.elapsed().as_millis() as u64;
    let output_state = session.state.clone();
    let session =
        std::sync::Arc::try_unwrap(session).map_err(|_| "evaluation session still shared")?;
    let result =
        tokio::task::spawn_blocking(move || output::assemble(session, work, elapsed, expired))
            .await
            .map_err(|e| e.to_string())?;
    // Teardown follows measurement and never converts cancellation into success.
    let probe_shutdown = output_state.probe.shutdown_workers().await;
    let frame_shutdown = output_state.chapter_frames().shutdown().await;
    probe_shutdown.map_err(|e| format!("probe shutdown failed: {e}"))?;
    frame_shutdown.map_err(|e| format!("frame shutdown failed: {e}"))?;
    result
}

fn associated_jobs(
    state: &crate::ApiState,
    sources: &[RegisteredSource],
    original: &[String],
) -> Result<Vec<String>, String> {
    let mut jobs = original
        .iter()
        .map(|id| {
            state
                .store
                .get_probe_job(id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "evaluation job disappeared".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(since) = jobs.iter().map(|job| job.created_at_ms).min() {
        for source in sources {
            let mut after = None;
            loop {
                let page = state
                    .store
                    .probe_jobs_for_file_since(
                        &source.row.file.file_id.to_string(),
                        since,
                        after,
                        16,
                    )
                    .map_err(|e| e.to_string())?;
                let Some(last) = page.last() else { break };
                after = Some((last.created_at_ms, last.id.clone()));
                jobs.extend(page);
            }
        }
    }
    jobs.sort_by(|a, b| (a.created_at_ms, &a.id).cmp(&(b.created_at_ms, &b.id)));
    jobs.dedup_by(|a, b| a.id == b.id);
    Ok(jobs.into_iter().map(|job| job.id).collect())
}

async fn record_incomplete(state: crate::ApiState, work: &mut [MediaWork]) -> Result<(), String> {
    let sources = work
        .iter()
        .filter(|media| media.terminal.is_none())
        .map(|media| media.sources.clone())
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return Ok(());
    }
    let observations = tokio::task::spawn_blocking(move || {
        sources
            .iter()
            .map(|sources| inspect_frames(&state, sources))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;
    for (media, observation) in work
        .iter_mut()
        .filter(|media| media.terminal.is_none())
        .zip(observations)
    {
        let frames = observation?;
        media.terminal = Some(measurement(
            media,
            TerminalStatus::Incomplete,
            frames.targets,
            frames.ready,
        ));
    }
    Ok(())
}

fn measurement(
    media: &MediaWork,
    status: TerminalStatus,
    targets: usize,
    ready: usize,
) -> MediaMeasurement {
    MediaMeasurement {
        media_group: media.group.clone(),
        media_id: media.sources[0].row.file.media_id,
        file_count: media.sources.len(),
        independent_episode_count: media
            .sources
            .iter()
            .flat_map(|s| s.row.units.iter().map(|u| (u.season, u.episode)))
            .collect::<BTreeSet<_>>()
            .len(),
        elapsed_ms: media.started.elapsed().as_millis() as u64,
        wallclock_complete: status == TerminalStatus::Completed,
        terminal_status: status,
        job_ids: media.jobs.clone(),
        frame_targets: targets,
        ready_frames: ready,
    }
}

fn enqueue(session: &BenchmarkSession) -> Result<Vec<MediaWork>, String> {
    let mut groups: BTreeMap<String, Vec<RegisteredSource>> = BTreeMap::new();
    for source in &session.registered {
        groups
            .entry(source.media_group.clone())
            .or_default()
            .push(source.clone());
    }
    let mut work = Vec::new();
    for (group, sources) in groups {
        let mut media = MediaWork {
            group,
            sources,
            jobs: Vec::new(),
            started: Instant::now(),
            terminal: None,
            error: None,
        };
        match session.options.scenario.as_str() {
            "incremental" | "retry" => enqueue_individual(session, &mut media)?,
            _ => enqueue_refresh(session, &mut media)?,
        }
        work.push(media);
    }
    Ok(work)
}

fn enqueue_refresh(session: &BenchmarkSession, media: &mut MediaWork) -> Result<(), String> {
    if (session.options.scenario == "cold" && media.sources.iter().any(|s| !s.is_new))
        || (session.options.scenario == "warm" && media.sources.iter().any(|s| s.is_new))
    {
        media.error = Some("scenario requires cold or previously completed source state".into());
        return Ok(());
    }
    let mut seasons: BTreeMap<i32, Vec<ProbeUnit>> = BTreeMap::new();
    for source in &media.sources {
        seasons
            .entry(source.row.units[0].season)
            .or_default()
            .push(ProbeUnit {
                row: source.row.clone(),
                kind: domain::MediaKind::Tv,
                force_fingerprint: true,
                reuse_fingerprint_cache: true,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            });
    }
    for (season, units) in seasons {
        let id = media.sources[0].row.file.media_id.to_string();
        let before = session
            .state
            .store
            .latest_marker_refresh_for_season(&id, season as u32)
            .map_err(|e| e.to_string())?;
        session
            .state
            .probe
            .enqueue_marker_refresh_with_policy(units, session.options.capture_policy);
        match session
            .state
            .store
            .latest_marker_refresh_for_season(&id, season as u32)
            .map_err(|e| e.to_string())?
        {
            Some(job) if before.as_ref().is_none_or(|old| old.id != job.id) => {
                media.jobs.push(job.id)
            }
            _ => media.error = Some("refresh did not create a new production job".into()),
        }
    }
    Ok(())
}

fn enqueue_individual(session: &BenchmarkSession, media: &mut MediaWork) -> Result<(), String> {
    let incremental = session.options.scenario == "incremental";
    if incremental
        && (!media.sources.iter().any(|s| s.is_new) || media.sources.iter().all(|s| s.is_new))
    {
        media.error = Some("incremental requires stable existing sources and new arrivals".into());
        return Ok(());
    }
    for source in &media.sources {
        if incremental && !source.is_new {
            continue;
        }
        match session
            .state
            .probe
            .request_probe(
                &source.row,
                if incremental {
                    ProbeRequestOrigin::Filesystem
                } else {
                    ProbeRequestOrigin::ManualRetry
                },
            )
            .map_err(|e| e.to_string())?
        {
            ProbeRequestResult::Queued { job_id } => media.jobs.push(job_id),
            other => {
                if !incremental && enqueue_frame_retry(session, source)? {
                    continue;
                }
                media.error = Some(format!(
                    "requested operation did not enqueue work: {other:?}"
                ));
            }
        }
    }
    Ok(())
}

fn enqueue_frame_retry(
    session: &BenchmarkSession,
    source: &RegisteredSource,
) -> Result<bool, String> {
    let task = session
        .state
        .store
        .current_chapter_frame_task(source.row.file.file_id)
        .map_err(|e| e.to_string())?;
    let Some(task) = task else { return Ok(false) };
    if !matches!(
        task.status.state,
        store::ChapterFrameState::Failed | store::ChapterFrameState::Partial
    ) {
        return Ok(false);
    }
    session
        .state
        .chapter_frames()
        .enqueue_path(&source.row.file.path, task.task.targets, true)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

fn inspect(
    state: &crate::ApiState,
    sources: &[RegisteredSource],
    jobs: &[String],
    error: Option<&str>,
) -> Result<Option<(TerminalStatus, usize, usize)>, String> {
    let mut failed = error.is_some();
    for id in jobs {
        let job = state
            .store
            .get_probe_job(id)
            .map_err(|e| e.to_string())?
            .ok_or("evaluation job disappeared")?;
        if job.is_active() {
            return Ok(None);
        }
        failed |= job.status != "succeeded";
    }
    if sources.iter().any(|s| {
        state
            .probe
            .holds_dispatch_id(&s.row.file.file_id.to_string())
    }) {
        return Ok(None);
    }
    for source in sources {
        if state
            .store
            .active_probe_unit_for_ledger(&source.row.file.file_id.to_string())
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Ok(None);
        }
    }
    let frames = inspect_frames(state, sources)?;
    if frames.active
        || sources.iter().any(|source| {
            state
                .chapter_frames()
                .has_pending_file(source.row.file.file_id)
        })
    {
        return Ok(None);
    }
    Ok(Some((
        if failed || frames.failed {
            TerminalStatus::Failed
        } else {
            TerminalStatus::Completed
        },
        frames.targets,
        frames.ready,
    )))
}

#[derive(Default)]
struct FrameObservation {
    active: bool,
    failed: bool,
    targets: usize,
    ready: usize,
}

fn inspect_frames(
    state: &crate::ApiState,
    sources: &[RegisteredSource],
) -> Result<FrameObservation, String> {
    let mut frames = FrameObservation::default();
    for source in sources {
        let expected = state
            .store
            .current_chapter_frame_targets(source.row.file.file_id)
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        frames.targets += expected.len();
        let task = state
            .store
            .current_chapter_frame_task(source.row.file.file_id)
            .map_err(|e| e.to_string())?;
        match task {
            Some(task) => {
                frames.active |= matches!(
                    task.status.state,
                    store::ChapterFrameState::Queued | store::ChapterFrameState::Running
                );
                frames.failed |= task.status.state != store::ChapterFrameState::Completed
                    || task.targets.len() != expected.len();
                for target in task.targets {
                    let valid = target
                        .published
                        .as_ref()
                        .filter(|_| target.ready)
                        .is_some_and(|artifact| {
                            let path = crate::chapter_frame_service::chapter_frame_artifact_path(
                                &source.row.file.path,
                                &task.status.generation,
                                target.index,
                                artifact,
                            );
                            image::open(path).is_ok()
                        });
                    if valid {
                        frames.ready += 1;
                    } else {
                        frames.failed = true;
                    }
                }
            }
            None => frames.failed |= !expected.is_empty(),
        }
    }
    Ok(frames)
}
