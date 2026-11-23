//! Low priority fingerprint processing and deferred marker publication.
use super::{
    FingerprintWindowGuard, is_temporary_busy, refresh_leases, retry_dispatch, source_ready,
    start_persisted_unit, step, wait_for_shutdown, work_lease,
};
use crate::probe_manager::{ProbeManager, probe, stages};
use std::sync::Arc;

pub(super) async fn run_fingerprint_worker(manager: Arc<ProbeManager>) {
    loop {
        let message = {
            let mut rx = manager.fingerprint_rx.lock().take();
            let message = match rx.as_mut() {
                Some(receiver) => {
                    let received = tokio::select! {
                        _ = wait_for_shutdown(&manager) => Some(None),
                        message = receiver.recv() => Some(message),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => None,
                    };
                    match received {
                        Some(message) => message,
                        None => {
                            *manager.fingerprint_rx.lock() = rx;
                            continue;
                        }
                    }
                }
                None => None,
            };
            if message.is_some() {
                *manager.fingerprint_rx.lock() = rx;
            }
            message
        };
        let Some((id, work)) = message else { break };
        let _window = FingerprintWindowGuard(manager.clone());
        if manager
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            manager.remove_seen_if_job(&id.ledger_id, &id.job_id);
            continue;
        }
        if manager.background_is_paused()
            && !manager.is_dispatch_paused_exempt(&id.job_id, &id.ledger_id)
        {
            manager.remove_seen_if_job(&id.ledger_id, &id.job_id);
            continue;
        }
        let Some(owns_running) = process_fingerprint(&manager, work).await else {
            continue;
        };
        retry_dispatch(&manager, &id, owns_running).await;
    }
}

async fn process_fingerprint(
    manager: &Arc<ProbeManager>,
    work: probe::FingerprintWork,
) -> Option<bool> {
    let owns_running = work.start_job;
    let unit = work.unit.clone();
    let ready = step(&manager, {
        let unit = unit.clone();
        move |manager| {
            let ledger_id = unit.row.file.file_id.to_string();
            if let Some(job_id) = unit.job_id.as_deref()
                && manager.background_is_paused()
                && !manager.is_dispatch_paused_exempt(job_id, &ledger_id)
            {
                manager.remove_seen_if_job(&ledger_id, job_id);
                return Err(());
            }
            Ok(source_ready(manager, &unit)
                && (!work.start_job
                    || start_persisted_unit(
                        manager,
                        &unit,
                        store::ProbeDispatchStage::Fingerprint,
                    )))
        }
    })
    .await;
    match ready {
        Ok(Ok(true)) => {}
        Ok(Err(())) => return None,
        Ok(Ok(false)) | Err(_) => return Some(false),
    }
    let leased = step(&manager, {
        let unit = unit.clone();
        move |manager| {
            if unit.marker_refresh_id.is_some() {
                refresh_leases(manager, &unit)
            } else {
                work_lease(manager, &unit, true).map(|lease| lease.into_iter().collect())
            }
        }
    })
    .await;
    let _refresh_leases = match leased {
        Ok(Ok(leases)) => leases,
        Ok(Err(reason)) => {
            if !is_temporary_busy(&reason) {
                let _ = step(manager, {
                    let unit = unit.clone();
                    move |manager| manager.cancel_unit(&unit, &reason.to_string())
                })
                .await;
            }
            return Some(owns_running);
        }
        Err(_) => return Some(owns_running),
    };
    let owner = unit.row.file.library_id.to_string();
    let control = _refresh_leases
        .iter()
        .find(|lease| lease.library_id() == owner)
        .map(|lease| lease.control().signal().clone())
        .unwrap_or_else(|| {
            let control = marker::WorkControl::default();
            if manager.library_work.lock().is_some() {
                control.cancel();
            }
            control
        })
        .with_read_stop(manager.remote_reads_wait_stopped.clone());
    // The full-window and adaptive pipelines include synchronous source reads
    // and SQL between awaits. Drive the whole future on one bounded worker;
    // the permit stays with it until its final publication has really finished.
    let runtime = tokio::runtime::Handle::current();
    let outcome = match step(manager, move |manager| {
        runtime.block_on(probe::probe_fingerprint_detailed(manager, &work, &control))
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(%error, "声纹流水线 worker 失败，保留持久单元供重试");
            return Some(owns_running);
        }
    };
    let _ = step(&manager, move |manager| {
        if manager
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
            || marker::media_read::remote_reads_stopping()
        {
            return;
        }
        if !source_ready(manager, &unit) {
            return;
        }
        finish_fingerprint_outcome(manager, &unit, outcome);
    })
    .await;
    Some(owns_running)
}

fn finish_fingerprint_outcome(
    manager: &ProbeManager,
    unit: &crate::probe_manager::ProbeUnit,
    outcome: stages::FingerprintRunOutcome,
) {
    match outcome {
        stages::FingerprintRunOutcome::Retry => {}
        stages::FingerprintRunOutcome::Uncertain { reason } => {
            manager.finish_with_status(
                unit,
                "succeeded",
                Some("verification_uncertain"),
                Some(&reason),
            );
        }
        stages::FingerprintRunOutcome::Complete => {
            if let Err(error) = crate::probe_manager::analysis::handoff(manager, unit) {
                if crate::probe_manager::store_access::is_busy(&error) {
                    tracing::warn!(%error, job_id = ?unit.job_id, "持久分析交接繁忙，保留采集单元等待重试");
                } else if matches!(error, store::StoreError::Missing(_)) {
                    tracing::warn!(%error, job_id = ?unit.job_id, "持久分析交接源已缺失，取消单元");
                    manager.cancel_unit(unit, &error.to_string());
                } else {
                    let error_kind = match error {
                        store::StoreError::Invalid(_) => "analysis_handoff_invalid",
                        _ => "analysis_handoff_rejected",
                    };
                    tracing::error!(%error, job_id = ?unit.job_id, error_kind, "持久分析交接永久失败，终止单元");
                    manager.finish_with_status(
                        unit,
                        "failed",
                        Some(error_kind),
                        Some(&error.to_string()),
                    );
                }
            }
        }
        stages::FingerprintRunOutcome::Partial { error_kind, error } => {
            manager.finish_with_status(unit, "partial", Some(&error_kind), Some(&error));
        }
        stages::FingerprintRunOutcome::Failed { error_kind, error } => {
            manager.finish_with_status(unit, "failed", Some(&error_kind), Some(&error));
        }
        stages::FingerprintRunOutcome::Cancelled { reason } => {
            manager.cancel_unit(unit, &reason);
        }
    }
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
