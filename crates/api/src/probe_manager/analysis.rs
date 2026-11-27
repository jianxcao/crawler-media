//! Independent analysis consumer; capture slots never wait for quiet time.
mod capture;
mod compute;
mod intro_search;
mod models;
mod plan_validation;
use super::{ProbeManager, ProbeUnit, season_schedule::SeasonKey};
use crate::ScrapeStoreExt;
pub(super) use capture::{capture_target, prepare_plan};
use std::sync::{Arc, atomic::Ordering};

fn metadata_duration(
    manager: &ProbeManager,
    row: &domain::FileCoverage,
) -> Result<Option<i64>, store::StoreError> {
    Ok(manager
        .store
        .get_media_info_cache_version(&row.file.file_id.to_string())?
        .filter(|metadata| metadata.source_version == row.file.version.source_version)
        .and_then(|metadata| metadata.format_duration_ms)
        .filter(|duration| *duration > 0))
}

fn prepare_handoff_participant(
    manager: &ProbeManager,
    unit: &ProbeUnit,
    id: &str,
    job: &str,
) -> Result<Option<store::ProbeAnalysisParticipant>, store::StoreError> {
    let plan = manager
        .store
        .active_probe_unit_for_ledger(id)?
        .and_then(|u| u.sampling_plan_json)
        .ok_or_else(|| {
            store::StoreError::Invalid("capture plan missing at analysis handoff".into())
        })?;
    let Some(plan) = capture::CapturePlan::consume(
        manager,
        unit,
        &plan,
        metadata_duration(manager, &unit.row)?,
    )?
    else {
        return Ok(None);
    };
    let mode = plan.sampling_mode;
    let cache = manager.store.get_fingerprint_cache(id)?;
    let duration = manager
        .store
        .get_media_info_cache_version(id)?
        .and_then(|v| v.format_duration_ms);
    let key = if mode == "adaptive" {
        crate::fingerprint_job::key_for_file(
            &manager.store,
            &unit.row.file,
            plan.policy.full_window_duration_secs,
            duration,
        )
    } else {
        cache
            .as_ref()
            .map(|cache| cache.cache_key.clone())
            .unwrap_or_default()
    };
    let required = if duration.is_some() {
        vec!["intro".into(), "outro".into()]
    } else {
        vec!["intro".into()]
    };
    let row = manager
        .store
        .file_coverage_by_id(unit.row.file.file_id)?
        .ok_or_else(|| store::StoreError::Missing("analysis source missing".into()))?;
    if row.file.version != unit.row.file.version
        || row.file.path != unit.row.file.path
        || row.file.media_id != unit.row.file.media_id
        || row.file.library_id != unit.row.file.library_id
        || row.file.root_id != unit.row.file.root_id
        || row.units != unit.row.units
        || row.file.presence != domain::FilePresence::Present
        || row.file.publication != domain::FilePublication::Published
    {
        return Err(store::StoreError::Protected(
            "analysis target changed".into(),
        ));
    }
    let request = store::ProbeAnalysisRequest {
        dispatch: store::ProbeDispatchId {
            job_id: job.to_string(),
            ledger_id: id.to_string(),
            stage: store::ProbeDispatchStage::Fingerprint,
        },
        target: row,
        season: super::require_episode_coordinates(&unit.row)?.0,
        sampling_mode: mode,
        analysis_policy_key: crate::fingerprint_job::analysis_policy_key(&plan.policy),
        analysis_policy_json: serde_json::to_string(&plan.policy)?,
        capture_context_key: key,
        required_kinds: required,
    };
    let prepared = manager.store.prepare_probe_analysis(&request)?;
    let prepared = if request.sampling_mode == "adaptive" {
        prepared.select_samples(&plan.selected_samples)?
    } else {
        prepared
    };
    Ok(Some(prepared))
}

pub(super) fn handoff(manager: &ProbeManager, unit: &ProbeUnit) -> Result<(), store::StoreError> {
    let id = unit.row.file.file_id.to_string();
    let job = unit
        .job_id
        .as_ref()
        .ok_or_else(|| store::StoreError::Invalid("analysis job missing".into()))?;
    let Some(prepared) = prepare_handoff_participant(manager, unit, &id, job)? else {
        return Ok(());
    };
    let dirty_at = manager.clock.lock().now_ms();
    if !manager.store.advance_probe_unit_to_analysis(&prepared)? {
        return Err(store::StoreError::Protected(
            "analysis handoff rejected".into(),
        ));
    }
    manager.remove_seen_if_job(&id, job);
    if manager.fingerprint_dispatch_paused()
        && !manager.is_job_paused_exempt(job)
        && manager.is_unit_paused_exempt(job, &id)
        && manager
            .store
            .get_probe_job(job)?
            .is_some_and(|row| row.kind == "marker_refresh")
        && manager
            .store
            .probe_analysis_job_participants(job)?
            .is_none()
    {
        manager.unmark_unit_paused_exempt(job, &id);
    }
    if !manager.fingerprint_dispatch_paused() || manager.is_dispatch_paused_exempt(job, &id) {
        observe_dirty(
            manager,
            SeasonKey {
                media_id: prepared.context().media_id,
                season: prepared.context().season,
            },
            dirty_at,
        )?;
        manager.wake.notify_waiters();
    }
    Ok(())
}

pub(super) fn season_has_paused_exempt_analysis(
    manager: &ProbeManager,
    media: domain::MediaId,
    season: u32,
) -> Result<bool, store::StoreError> {
    let mut after: Option<(String, String)> = None;
    let mut refresh_runnable = std::collections::HashMap::<String, Option<bool>>::new();
    loop {
        let rows = manager.store.probe_analysis_participants(
            media,
            season,
            after.as_ref().map(|(j, f)| (j.as_str(), f.as_str())),
            16,
        )?;
        if rows.is_empty() {
            return Ok(false);
        }
        for row in &rows {
            let job_id = &row.dispatch().job_id;
            let lid = &row.dispatch().ledger_id;
            if manager.is_unit_paused_exempt(job_id, lid) {
                return Ok(true);
            }
            let entry = match refresh_runnable.get(job_id) {
                Some(&cached) => cached,
                None => {
                    let computed = match manager.store.get_probe_job(job_id)? {
                        Some(job) if job.kind == "marker_refresh" => {
                            let ready = manager
                                .store
                                .probe_analysis_job_participants(job_id)?
                                .is_some()
                                || manager
                                    .store
                                    .probe_job_units(job_id)?
                                    .iter()
                                    .any(|u| u.status == "failed" || u.status == "partial");
                            Some(ready)
                        }
                        _ => None,
                    };
                    refresh_runnable.insert(job_id.clone(), computed);
                    computed
                }
            };
            match entry {
                None if manager.is_job_paused_exempt(job_id) => return Ok(true),
                Some(true)
                    if manager.is_job_paused_exempt(job_id)
                        || manager.job_has_unit_exemption_or_promotion(job_id) =>
                {
                    return Ok(true);
                }
                _ => {}
            }
        }
        let Some(last) = rows.last() else {
            return Ok(false);
        };
        after = Some((
            last.dispatch().job_id.clone(),
            last.dispatch().ledger_id.clone(),
        ));
    }
}

pub(super) fn season_analysis_digest(
    manager: &ProbeManager,
    media: domain::MediaId,
    season: u32,
) -> Result<Option<String>, store::StoreError> {
    let mut keys = Vec::new();
    let mut after: Option<(String, String)> = None;
    loop {
        let rows = manager.store.probe_analysis_participants(
            media,
            season,
            after.as_ref().map(|(j, f)| (j.as_str(), f.as_str())),
            16,
        )?;
        let Some(last) = rows.last() else {
            break;
        };
        after = Some((
            last.dispatch().job_id.clone(),
            last.dispatch().ledger_id.clone(),
        ));
        for row in rows {
            keys.push(row.context().context_key.clone());
        }
    }
    Ok((!keys.is_empty()).then(|| keys.join("|")))
}

fn scan_analysis_keys(
    manager: &ProbeManager,
    after: Option<(domain::MediaId, u32)>,
) -> Result<(Option<(domain::MediaId, u32)>, Vec<(SeasonKey, String)>), store::StoreError> {
    let paused = manager.fingerprint_dispatch_paused();
    if paused && !manager.has_paused_exempt_analysis_work() {
        return Ok((None, Vec::new()));
    }
    let keys = manager.store.pending_probe_analysis_keys(after, 16)?;
    let mut result = Vec::new();
    for (media, season) in &keys {
        if paused && !season_has_paused_exempt_analysis(manager, *media, *season)? {
            continue;
        }
        if let Some(digest) = season_analysis_digest(manager, *media, *season)? {
            result.push((
                SeasonKey {
                    media_id: *media,
                    season: *season,
                },
                digest,
            ));
        }
    }
    Ok((keys.last().copied(), result))
}

async fn execute_due_ticket(
    manager: &Arc<ProbeManager>,
    ticket: super::season_schedule::AnalysisTicket,
) {
    let key = ticket.key;
    let is_exempt = super::worker::step(manager, move |manager| {
        season_has_paused_exempt_analysis(manager, key.media_id, key.season).unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if !is_exempt {
        super::worker::wait_for_metadata_idle(manager).await;
    }
    let result = super::worker::step(manager, move |manager| {
        if manager.fingerprint_dispatch_paused()
            && !season_has_paused_exempt_analysis(manager, key.media_id, key.season)?
        {
            return Ok(None);
        }
        compute::run(manager, key)
    })
    .await;
    let mut continuation = None;
    match result {
        Ok(Ok(next)) => continuation = next,
        Ok(Err(error)) => {
            tracing::warn!(%error, media_id = %key.media_id, season = key.season, "分析未提交，保留持久工作")
        }
        Err(error) => tracing::error!(%error, "分析 worker 失败"),
    }
    let now = manager.clock.lock().now_ms();
    let mut schedule = manager.analysis_schedule.lock();
    schedule.finish(&ticket, now);
    if let Some((digest, unchanged)) = continuation {
        schedule.mark_dirty(
            key,
            digest,
            if unchanged {
                now.saturating_sub(2_000)
            } else {
                now
            },
        );
    }
}

pub(super) async fn run(manager: Arc<ProbeManager>) {
    let mut after = None;
    loop {
        if manager.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        if manager.fingerprint_dispatch_paused() && !manager.has_paused_exempt_analysis_work() {
            after = None;
            tokio::select! {
                _ = super::worker::wait_for_shutdown(&manager) => break,
                _ = manager.wake.notified() => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
            }
            continue;
        }
        match super::worker::step(&manager, move |manager| scan_analysis_keys(manager, after)).await
        {
            Ok(Ok((cursor, rows))) => {
                after = cursor;
                let now = manager.clock.lock().now_ms();
                for (key, digest) in rows {
                    manager
                        .analysis_schedule
                        .lock()
                        .mark_dirty(key, digest, now);
                }
            }
            Ok(Err(error)) => tracing::warn!(%error, "分析窗口读取失败，持久工作保持排队"),
            Err(error) => tracing::error!(%error, "分析窗口 worker 失败"),
        }
        let now = manager.clock.lock().now_ms();
        let ticket = manager.analysis_schedule.lock().take_due(now);
        if let Some(ticket) = ticket {
            execute_due_ticket(&manager, ticket).await;
            continue;
        }
        tokio::select! {
            _ = super::worker::wait_for_shutdown(&manager) => break,
            _ = manager.wake.notified() => {},
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
        }
    }
}

pub(super) fn observe_dirty(
    manager: &ProbeManager,
    key: SeasonKey,
    dirty_at: i64,
) -> Result<(), store::StoreError> {
    if let Some(digest) = season_analysis_digest(manager, key.media_id, key.season)? {
        manager
            .analysis_schedule
            .lock()
            .mark_dirty(key, digest, dirty_at);
    }
    Ok(())
}

pub(super) fn observe_dirty_participants(
    manager: &ProbeManager,
    participants: &[store::ProbeAnalysisParticipant],
) -> Result<(), store::StoreError> {
    let dirty_at = manager.clock.lock().now_ms();
    let keys = participants
        .iter()
        .map(|p| SeasonKey {
            media_id: p.context().media_id,
            season: p.context().season,
        })
        .collect::<std::collections::BTreeSet<_>>();
    for key in keys {
        observe_dirty(manager, key, dirty_at)?;
    }
    Ok(())
}
