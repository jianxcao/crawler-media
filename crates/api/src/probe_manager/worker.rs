use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{ProbeManager, ProbeUnit, probe};

async fn recv_id(
    slot: &parking_lot::Mutex<Option<tokio::sync::mpsc::Receiver<store::ProbeDispatchId>>>,
) -> Option<store::ProbeDispatchId> {
    let mut rx = slot.lock().take()?;
    let id = rx.recv().await;
    if id.is_some() {
        *slot.lock() = Some(rx);
    }
    id
}
pub(super) fn try_start(manager: &Arc<ProbeManager>) -> bool {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("【媒体探测】当前不在 Tokio runtime 上下文，探测 worker 未启动");
        return false;
    }
    if manager.workers_started.swap(true, Ordering::SeqCst) {
        tracing::info!("【媒体探测】worker 已在本 runtime 启动，忽略重复启动");
        manager.wake.notify_waiters();
        return true;
    }
    let metadata_workers = manager
        .store
        .lock()
        .get_setting(crate::settings_keys::PROBE_CONCURRENCY)
        .ok()
        .flatten()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
        .min(8);
    let mut workers = manager.workers.lock();
    for _ in 0..metadata_workers {
        let manager = manager.clone();
        workers.push(tokio::spawn(run_metadata_worker(manager)));
    }
    let fingerprint = manager.clone();
    workers.push(tokio::spawn(run_fingerprint_worker(fingerprint)));
    let dispatcher = manager.clone();
    workers.push(tokio::spawn(run_window_dispatcher(dispatcher)));
    drop(workers);
    manager.fill_dispatch_windows();
    manager.wake.notify_waiters();
    tracing::info!(
        metadata_workers,
        fingerprint_workers = 1,
        "【媒体探测】高优先级媒体信息队列与低优先级声纹队列已启动"
    );
    true
}

/// Run one driver step on the blocking pool (Task 22 / Review L2).
///
/// The probe workers are tasks on the request runtime, so a `Store` lock taken in
/// their async bodies parks a worker and stalls every other task — `/health`
/// included, which touches no store at all. Waiting on a blocking thread costs a
/// thread, never the executor. `Err` is a worker panic; the durable dispatch row is
/// untouched either way, so the unit is simply retried by the next window.
async fn step<T, F>(manager: &Arc<ProbeManager>, work: F) -> Result<T, String>
where
    F: FnOnce(&ProbeManager) -> T + Send + 'static,
    T: Send + 'static,
{
    let manager = manager.clone();
    tokio::task::spawn_blocking(move || work(&manager))
        .await
        .map_err(|error| format!("probe worker step failed: {error}"))
}

async fn run_metadata_worker(manager: Arc<ProbeManager>) {
    loop {
        let id = recv_id(&manager.metadata_rx).await;

        let Some(id) = id else { break };
        if manager.shutting_down.load(Ordering::SeqCst) {
            manager.seen.lock().remove(&id.ledger_id);
            manager.metadata_stage_finished();
            continue;
        }
        let loaded = step(&manager, {
            let id = id.clone();
            move |manager| manager.load_dispatch_unit(&id)
        })
        .await;
        let Some((unit, _)) = loaded.ok().flatten() else {
            let _ = step(&manager, {
                let id = id.clone();
                move |manager| cancel_deleted_dispatch(manager, &id)
            })
            .await;
            manager.metadata_stage_finished();
            let _ = step(&manager, |manager| manager.refill_drained_windows()).await;
            continue;
        };
        let pending = MetadataStageGuard(manager.clone());
        let leased = step(&manager, {
            let unit = unit.clone();
            move |manager| work_lease(manager, &unit)
        })
        .await;
        let _lease = match leased {
            Ok(Ok(lease)) => lease,
            Ok(Err(reason)) => {
                let _ = step(&manager, {
                    let unit = unit.clone();
                    let reason = reason.clone();
                    move |manager| manager.cancel_unit(&unit, &reason)
                })
                .await;
                continue;
            }
            Err(error) => {
                tracing::error!(%error, "探测工作租约步骤失败，单元留在持久队列");
                continue;
            }
        };
        let ready = step(&manager, {
            let unit = unit.clone();
            move |manager| source_ready(manager, &unit) && start_persisted_unit(manager, &unit)
        })
        .await;
        if !ready.unwrap_or(false) {
            continue;
        }
        let _refresh_leases = if unit.marker_refresh_id.is_some() {
            let leases = step(&manager, {
                let unit = unit.clone();
                move |manager| refresh_leases(manager, &unit)
            })
            .await;
            match leases {
                Ok(Ok(leases)) => leases,
                Ok(Err(reason)) => {
                    let _ = step(&manager, {
                        let unit = unit.clone();
                        move |manager| manager.cancel_unit(&unit, &reason)
                    })
                    .await;
                    continue;
                }
                Err(error) => {
                    tracing::error!(%error, "探测刷新租约步骤失败，单元留在持久队列");
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        let outcome = probe::probe_metadata(&manager, &unit).await;
        drop(pending);
        let _ = step(&manager, |manager| manager.refill_drained_windows()).await;
        let still_ready = step(&manager, {
            let unit = unit.clone();
            move |manager| source_ready(manager, &unit)
        })
        .await
        .unwrap_or(false);
        if !still_ready {
            continue;
        }
        match outcome {
            probe::MetadataOutcome::Complete => {
                let _ = step(&manager, {
                    let unit = unit.clone();
                    move |manager| manager.finish(&unit, true)
                })
                .await;
            }
            probe::MetadataOutcome::Failed => {
                let _ = step(&manager, {
                    let unit = unit.clone();
                    move |manager| manager.finish(&unit, false)
                })
                .await;
            }
            probe::MetadataOutcome::Fingerprint(work) => {
                let _ = step(&manager, move |manager| {
                    manager.enqueue_fingerprint_after_metadata(work)
                })
                .await;
            }
        }
    }
}
fn cancel_deleted_dispatch(manager: &ProbeManager, id: &store::ProbeDispatchId) {
    let missing = super::ledger_id(&id.ledger_id)
        .and_then(|file_id| {
            manager
                .store
                .lock()
                .file_coverage_by_id(file_id)
                .ok()
                .flatten()
        })
        .is_none();
    if !missing {
        return;
    }
    if let Err(error) =
        manager
            .store
            .lock()
            .cancel_probe_unit(&id.job_id, &id.ledger_id, "file_deleted")
    {
        tracing::error!(%error, job_id = %id.job_id, ledger_id = %id.ledger_id, "删除文件后取消探测单元失败");
        return;
    }
    let refresh = manager
        .store
        .lock()
        .get_probe_job(&id.job_id)
        .ok()
        .flatten()
        .is_some_and(|job| job.kind == "marker_refresh" && job.is_active());
    if refresh
        && let Err(error) = manager
            .store
            .lock()
            .cancel_probe_job(&id.job_id, "file_deleted")
        {
            tracing::error!(%error, job_id = %id.job_id, "删除文件后取消刷新任务失败");
        }
    manager.seen.lock().remove(&id.ledger_id);
}

struct MetadataStageGuard(Arc<ProbeManager>);

impl Drop for MetadataStageGuard {
    fn drop(&mut self) {
        self.0.metadata_stage_finished();
    }
}

/// Refill both durable windows after every fingerprint outcome.
///
/// This worker can be the only remaining consumer once metadata drains, so a slot freed
/// here must be offered the next durable id: otherwise work that hit a full fingerprint
/// window at admission time would never drain without a restart.
struct FingerprintWindowGuard(Arc<ProbeManager>);

impl Drop for FingerprintWindowGuard {
    fn drop(&mut self) {
        self.0.refill_drained_windows();
        self.0.wake.notify_waiters();
    }
}

/// Await the manager's wake signal, then refill the dispatch windows.
///
/// Every `wake.notify_waiters()` call site (window full, recovery, page admission) means
/// "durable ids may now fit again"; without a consumer those signals were dead code.
async fn run_window_dispatcher(manager: Arc<ProbeManager>) {
    loop {
        manager.wake.notified().await;
        if manager.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        manager.refill_drained_windows();
    }
}

async fn run_fingerprint_worker(manager: Arc<ProbeManager>) {
    loop {
        let recovered = {
            let mut list = manager.recovered_marker_refreshes.lock();
            if !list.is_empty() {
                Some(list.remove(0))
            } else {
                None
            }
        };
        if let Some((refreshed, unit)) = recovered {
            let _leases = match refresh_leases(&manager, &unit) {
                Ok(leases) => leases,
                Err(reason) => {
                    manager.cancel_unit(&unit, &reason);
                    continue;
                }
            };
            wait_for_metadata_idle(&manager).await;
            super::marker_jobs::apply_marker_refresh(&manager, &refreshed, &unit);
            super::timings::log_terminal_job(&manager.store, &manager.timings, &refreshed);
            continue;
        }

        let message = {
            let mut rx = manager.fingerprint_rx.lock().take();
            let message = match rx.as_mut() {
                Some(rx) => rx.recv().await,
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

        if !source_ready(&manager, &work.unit) {
            continue;
        }

        if work.start_job && !start_persisted_unit(&manager, &work.unit) {
            continue;
        }
        let _refresh_leases = if work.unit.marker_refresh_id.is_some() {
            match refresh_leases(&manager, &work.unit) {
                Ok(leases) => leases,
                Err(reason) => {
                    manager.cancel_unit(&work.unit, &reason);
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        let outcome = probe::probe_fingerprint_detailed(&manager, &work).await;
        if source_ready(&manager, &work.unit) {
            match outcome {
                super::stages::FingerprintRunOutcome::Complete => {
                    manager.finish_with_status(&work.unit, "succeeded", None, None);
                }
                super::stages::FingerprintRunOutcome::Partial { error_kind, error } => {
                    manager.finish_with_status(
                        &work.unit,
                        "partial",
                        Some(&error_kind),
                        Some(&error),
                    );
                }
                super::stages::FingerprintRunOutcome::Failed { error_kind, error } => {
                    manager.finish_with_status(
                        &work.unit,
                        "failed",
                        Some(&error_kind),
                        Some(&error),
                    );
                }
                super::stages::FingerprintRunOutcome::Cancelled { reason } => {
                    manager.cancel_unit(&work.unit, &reason);
                }
            }
        }
    }
}

fn refresh_leases(
    manager: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<Vec<crate::library_lifecycle::LibraryWorkLease>, String> {
    if manager.library_work.lock().is_none() {
        return Ok(Vec::new());
    }
    // Refresh expands its requested units to every row of the same Media.
    // Hold every original owner until publication, not only during capture.
    let rows = manager
        .store
        .lock()
        .list_file_coverage()
        .map_err(|e| e.to_string())?;
    let units: Vec<_> = rows
        .into_iter()
        .filter(|row| row.file.media_id == unit.row.file.media_id)
        .map(|row| {
            let mut expanded = unit.clone();
            expanded.row = row;
            expanded
        })
        .collect();
    super::progress::MetadataPriorityGate::for_rows(manager, &units).map(|(_, leases)| leases)
}

pub(super) fn work_lease(
    manager: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<Option<crate::library_lifecycle::LibraryWorkLease>, String> {
    let registry = manager.library_work.lock().clone();
    let Some(registry) = registry else {
        return Ok(None);
    };
    let library_id = {
        let store = manager.store.lock();
        crate::fingerprint_job::source::ensure_source_recorded(&store, &unit.row)?;
        unit.row.file.library_id.to_string()
    };
    registry.acquire(&manager.store, &library_id).map(Some)
}

pub(super) fn source_ready(manager: &ProbeManager, unit: &ProbeUnit) -> bool {
    if let Some(id) = &unit.job_id
        && manager
            .store
            .lock()
            .get_probe_job(id)
            .ok()
            .flatten()
            .is_some_and(|job| !job.is_active())
        {
            manager
                .seen
                .lock()
                .remove(&unit.row.file.file_id.to_string());
            return false;
        }
    let availability =
        crate::fingerprint_job::source::ensure_source_available(&manager.store, &unit.row);
    match availability {
        Ok(()) => true,
        Err(error) if crate::fingerprint_job::source::is_file_deleted(&error) => {
            manager.cancel_unit(unit, "file_deleted");
            false
        }
        Err(error) => {
            tracing::error!(%error, ledger_id = %unit.row.file.file_id, "检查探测源文件失败");
            manager.finish(unit, false);
            false
        }
    }
}

pub(super) async fn wait_for_metadata_idle(manager: &ProbeManager) {
    let generation = manager
        .metadata_idle_generation
        .load(std::sync::atomic::Ordering::Acquire);
    loop {
        if manager
            .metadata_pending
            .load(std::sync::atomic::Ordering::Acquire)
            == 0
            || manager
                .metadata_idle_generation
                .load(std::sync::atomic::Ordering::Acquire)
                != generation
        {
            return;
        }
        let notified = manager.metadata_idle.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if manager
            .metadata_pending
            .load(std::sync::atomic::Ordering::Acquire)
            == 0
            || manager
                .metadata_idle_generation
                .load(std::sync::atomic::Ordering::Acquire)
                != generation
        {
            return;
        }
        notified.await;
    }
}

fn start_persisted_unit(manager: &ProbeManager, unit: &ProbeUnit) -> bool {
    let ledger_id = unit.row.file.file_id.to_string();
    let started = match unit.job_id.as_deref() {
        Some(job_id) => manager.store.lock().start_probe_unit(job_id, &ledger_id),
        None => {
            tracing::error!(ledger_id, "队列单元缺少任务 ID，无法启动持久化任务");
            return false;
        }
    };
    match started {
        Ok(true) => true,
        Ok(false) => {
            tracing::warn!(ledger_id, "队列单元已被处理或数据库状态缺失，跳过重复执行");
            manager.seen.lock().remove(&ledger_id);
            false
        }
        Err(error) => {
            tracing::error!(%error, ledger_id, job_id = ?unit.job_id, "探测任务开始状态写入失败");
            manager.finish(unit, false);
            false
        }
    }
}
