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

pub(super) fn handoff(manager: &ProbeManager, unit: &ProbeUnit) -> Result<(), store::StoreError> {
    let id = unit.row.file.file_id.to_string();
    let job = unit
        .job_id
        .as_ref()
        .ok_or_else(|| store::StoreError::Invalid("analysis job missing".into()))?;
    let plan = manager
        .store
        .active_probe_unit_for_ledger(&id)?
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
        return Ok(());
    };
    let mode = plan.sampling_mode;
    let cache = manager.store.get_fingerprint_cache(&id)?;
    let duration = manager
        .store
        .get_media_info_cache_version(&id)?
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
    let request = store::ProbeAnalysisRequest {
        dispatch: store::ProbeDispatchId {
            job_id: job.clone(),
            ledger_id: id.clone(),
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
    let dirty_at = manager.clock.lock().now_ms();
    if !manager.store.advance_probe_unit_to_analysis(&prepared)? {
        return Err(store::StoreError::Protected(
            "analysis handoff rejected".into(),
        ));
    }
    manager.seen.lock().remove(&id);
    observe_dirty(
        manager,
        SeasonKey {
            media_id: prepared.context().media_id,
            season: prepared.context().season,
        },
        dirty_at,
    )?;
    manager.wake.notify_waiters();
    Ok(())
}

pub(super) async fn run(manager: Arc<ProbeManager>) {
    let mut after = None;
    loop {
        if manager.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        let loaded = super::worker::step(&manager, move |manager| {
            let keys = manager.store.pending_probe_analysis_keys(after, 16)?;
            let mut result = Vec::new();
            for (media, season) in &keys {
                let rows = manager
                    .store
                    .probe_analysis_participants(*media, *season, None, 16)?;
                let digest = rows
                    .iter()
                    .map(|row| row.context().context_key.as_str())
                    .collect::<Vec<_>>()
                    .join("|");
                if !rows.is_empty() {
                    result.push((
                        SeasonKey {
                            media_id: *media,
                            season: *season,
                        },
                        digest,
                    ));
                }
            }
            Ok::<_, store::StoreError>((keys.last().copied(), result))
        })
        .await;
        match loaded {
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
            Ok(Err(error)) => tracing::warn!(%error,"分析窗口读取失败，持久工作保持排队"),
            Err(error) => tracing::error!(%error,"分析窗口 worker 失败"),
        }
        let now = manager.clock.lock().now_ms();
        let ticket = manager.analysis_schedule.lock().take_due(now);
        if let Some(ticket) = ticket {
            super::worker::wait_for_metadata_idle(&manager).await;
            let key = ticket.key;
            let result =
                super::worker::step(&manager, move |manager| compute::run(manager, key)).await;
            let mut continuation = None;
            match result {
                Ok(Ok(next)) => {
                    continuation = next;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error,media_id=%key.media_id,season=key.season,"分析未提交，保留持久工作")
                }
                Err(error) => tracing::error!(%error,"分析 worker 失败"),
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
            continue;
        }
        tokio::select! {
            _=super::worker::wait_for_shutdown(&manager)=>break,
            _=manager.wake.notified()=>{},
            _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{},
        }
    }
}

fn observe_dirty(
    manager: &ProbeManager,
    key: SeasonKey,
    dirty_at: i64,
) -> Result<(), store::StoreError> {
    let rows = manager
        .store
        .probe_analysis_participants(key.media_id, key.season, None, 16)?;
    if !rows.is_empty() {
        let digest = rows
            .iter()
            .map(|row| row.context().context_key.as_str())
            .collect::<Vec<_>>()
            .join("|");
        manager
            .analysis_schedule
            .lock()
            .mark_dirty(key, digest, dirty_at);
    }
    Ok(())
}
