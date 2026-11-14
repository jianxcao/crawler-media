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
            manager.seen.lock().remove(&id.ledger_id);
            continue;
        }
        let owns_running = process_fingerprint(&manager, work).await;
        retry_dispatch(&manager, &id, owns_running).await;
    }
}

async fn process_fingerprint(manager: &Arc<ProbeManager>, work: probe::FingerprintWork) -> bool {
    let owns_running = work.start_job;
    let unit = work.unit.clone();
    let ready = step(&manager, {
        let unit = unit.clone();
        move |manager| {
            source_ready(manager, &unit)
                && (!work.start_job || start_persisted_unit(manager, &unit))
        }
    })
    .await
    .unwrap_or(false);
    if !ready {
        return false;
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
                    move |manager| manager.cancel_unit(&unit, &reason)
                })
                .await;
            }
            return owns_running;
        }
        Err(_) => return owns_running,
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
        });
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
            return owns_running;
        }
    };
    let _ = step(&manager, move |manager| {
        if !source_ready(manager, &unit) {
            return;
        }
        match outcome {
            stages::FingerprintRunOutcome::Retry => {}
            stages::FingerprintRunOutcome::Uncertain { reason } => {
                manager.finish_with_status(
                    &unit,
                    "succeeded",
                    Some("verification_uncertain"),
                    Some(&reason),
                );
            }
            stages::FingerprintRunOutcome::Complete => {
                if let Err(error) = crate::probe_manager::analysis::handoff(manager, &unit) {
                    tracing::error!(%error,job_id=?unit.job_id,"持久分析交接失败，保留采集单元");
                }
            }
            stages::FingerprintRunOutcome::Partial { error_kind, error } => {
                manager.finish_with_status(&unit, "partial", Some(&error_kind), Some(&error));
            }
            stages::FingerprintRunOutcome::Failed { error_kind, error } => {
                manager.finish_with_status(&unit, "failed", Some(&error_kind), Some(&error));
            }
            stages::FingerprintRunOutcome::Cancelled { reason } => {
                manager.cancel_unit(&unit, &reason);
            }
        }
    })
    .await;
    owns_running
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
