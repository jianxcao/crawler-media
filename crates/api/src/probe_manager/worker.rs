use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{ProbeManager, ProbeUnit, probe};

async fn recv_id(manager: &ProbeManager) -> Option<store::ProbeDispatchId> {
    // Only the current receive owns the channel. Other workers await ownership
    // instead of interpreting an temporarily empty receiver slot as shutdown.
    let _receiver = tokio::select! {
        _ = wait_for_shutdown(manager) => return None,
        guard = manager.metadata_receiver.lock() => guard,
    };
    let slot = &manager.metadata_rx;
    let mut rx = slot.lock().take()?;
    let id = tokio::select! {
        _ = wait_for_shutdown(manager) => None,
        id = rx.recv() => id,
    };
    if id.is_some() {
        *slot.lock() = Some(rx);
    }
    id
}

async fn wait_for_shutdown(manager: &ProbeManager) {
    let mut signal = manager.shutdown_signal.subscribe();
    loop {
        if *signal.borrow_and_update() {
            return;
        }
        if signal.changed().await.is_err() {
            return;
        }
    }
}
pub(super) fn try_start(manager: &Arc<ProbeManager>) -> bool {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("【媒体探测】当前不在 Tokio runtime 上下文，探测 worker 未启动");
        return false;
    }
    let mut workers = manager.workers.lock();
    if manager.shutting_down.load(Ordering::SeqCst) {
        return false;
    }
    if manager.workers_started.load(Ordering::SeqCst) {
        tracing::info!("【媒体探测】worker 已在本 runtime 启动，忽略重复启动");
        manager.wake.notify_waiters();
        return true;
    }
    let Some(lease) = super::store_lease::acquire(manager.store_key.clone()) else {
        tracing::warn!("同一数据库的探测 worker 仍在运行，暂不启动新的 worker");
        return false;
    };
    *manager.store_lease.lock() = Some(lease);
    let recovery_deferred = manager.recovery_deferred.swap(false, Ordering::SeqCst);
    manager.workers_started.store(true, Ordering::SeqCst);
    manager.shutdown_signal.send_replace(false);
    let bootstrap = manager.clone();
    let handle = tokio::spawn(async move {
        if recovery_deferred {
            if let Err(error) = step(&bootstrap, |manager| manager.recover_pending_jobs()).await {
                tracing::error!(%error, "延迟恢复探测任务失败");
                bootstrap.recovery_deferred.store(true, Ordering::SeqCst);
                return;
            }
            if bootstrap.shutting_down.load(Ordering::SeqCst) {
                return;
            }
        }
        let configured = step(&bootstrap, |manager| {
            manager
                .store
                .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
                .and_then(|store| {
                    store
                        .get_setting(crate::settings_keys::PROBE_CONCURRENCY)
                        .ok()
                        .flatten()
                })
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1)
                .min(8)
        })
        .await;
        let Ok(metadata_workers) = configured else {
            tracing::error!("探测 worker 启动配置步骤失败");
            bootstrap.workers_started.store(false, Ordering::SeqCst);
            return;
        };
        if bootstrap.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        start_workers(&bootstrap, metadata_workers);
        if let Err(error) = step(&bootstrap, |manager| manager.fill_dispatch_windows()).await {
            tracing::error!(%error, "探测初始派发失败");
        }
    });
    workers.push(handle);
    true
}

fn start_workers(manager: &Arc<ProbeManager>, metadata_workers: usize) {
    let mut workers = manager.workers.lock();
    if manager.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    workers.push(tokio::spawn(run_fingerprint_worker(manager.clone())));
    workers.push(tokio::spawn(run_window_dispatcher(manager.clone())));
    for _ in 0..metadata_workers {
        let manager = manager.clone();
        workers.push(tokio::spawn(run_metadata_worker(manager)));
    }
    drop(workers);
    manager.wake.notify_waiters();
    tracing::info!(
        metadata_workers,
        fingerprint_workers = 1,
        "【媒体探测】高优先级媒体信息队列与低优先级声纹队列已启动"
    );
}

/// Run one driver step on the blocking pool (Task 22 / Review L2).
///
/// The probe workers are tasks on the request runtime, so a `Store` lock taken in
/// their async bodies parks a worker and stalls every other task — `/health`
/// included, which touches no store at all. Waiting on a blocking thread costs a
/// thread, never the executor. `Err` is a worker panic; the durable dispatch row is
/// untouched either way, so the unit is simply retried by the next window.
pub(super) async fn step<T, F>(manager: &Arc<ProbeManager>, work: F) -> Result<T, String>
where
    F: FnOnce(&ProbeManager) -> T + Send + 'static,
    T: Send + 'static,
{
    let manager = manager.clone();
    let budget = loop {
        match manager.dispatch_work.try_reserve() {
            Ok(budget) => break budget,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    };
    budget
        .run_to_completion(move || work(&manager))
        .await
        .map_err(|error| format!("probe worker step failed: {error}"))
}

async fn run_metadata_worker(manager: Arc<ProbeManager>) {
    loop {
        let id = recv_id(&manager).await;

        let Some(id) = id else { break };
        let pending = MetadataStageGuard(manager.clone());
        if manager.shutting_down.load(Ordering::SeqCst) {
            manager.seen.lock().remove(&id.ledger_id);
            continue;
        }
        let (handed_off, owns_running) = process_metadata(&manager, &id).await;
        drop(pending);
        if !handed_off {
            retry_dispatch(&manager, &id, owns_running).await;
        }
        let _ = step(&manager, |manager| manager.refill_drained_windows()).await;
    }
}

async fn process_metadata(
    manager: &Arc<ProbeManager>,
    id: &store::ProbeDispatchId,
) -> (bool, bool) {
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
        manager.wake.notify_one();
        return (true, false);
    };
    let leased = step(&manager, {
        let unit = unit.clone();
        move |manager| work_lease(manager, &unit)
    })
    .await;
    let _lease = match leased {
        Ok(Ok(lease)) => lease,
        Ok(Err(reason)) => {
            if !is_temporary_busy(&reason) {
                let _ = step(&manager, {
                    let unit = unit.clone();
                    let reason = reason.clone();
                    move |manager| manager.cancel_unit(&unit, &reason)
                })
                .await;
            }
            return (false, false);
        }
        Err(error) => {
            tracing::error!(%error, "探测工作租约步骤失败，单元留在持久队列");
            return (false, false);
        }
    };
    let ready = step(&manager, {
        let unit = unit.clone();
        move |manager| source_ready(manager, &unit) && start_persisted_unit(manager, &unit)
    })
    .await;
    if !ready.unwrap_or(false) {
        return (false, false);
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
                if !is_temporary_busy(&reason) {
                    let _ = step(&manager, {
                        let unit = unit.clone();
                        move |manager| manager.cancel_unit(&unit, &reason)
                    })
                    .await;
                }
                return (false, true);
            }
            Err(error) => {
                tracing::error!(%error, "探测刷新租约步骤失败，单元留在持久队列");
                return (false, true);
            }
        }
    } else {
        Vec::new()
    };
    let outcome = probe::probe_metadata(&manager, &unit).await;
    let still_ready = step(&manager, {
        let unit = unit.clone();
        move |manager| source_ready(manager, &unit)
    })
    .await
    .unwrap_or(false);
    if !still_ready {
        return (false, true);
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
        probe::MetadataOutcome::Retry => return (false, true),
        probe::MetadataOutcome::Fingerprint(work) => {
            let handed_off = step(&manager, move |manager| {
                manager.enqueue_fingerprint_after_metadata(work)
            })
            .await
            .unwrap_or(false);
            return (handed_off, true);
        }
    }
    (false, true)
}

fn is_temporary_busy(reason: &str) -> bool {
    matches!(
        reason,
        "probe Store busy" | "Library Store busy" | "Library Registry busy"
    )
}

async fn retry_dispatch(
    manager: &Arc<ProbeManager>,
    id: &store::ProbeDispatchId,
    owns_running: bool,
) {
    loop {
        let result = step(manager, {
            let id = id.clone();
            move |manager| {
                let mut seen = manager.seen.lock();
                // Never wait for Store while holding seen: other admission paths
                // can need both locks, and the retry loop can yield between attempts.
                let store = manager.store.try_lock().ok_or(())?;
                let active =
                    store
                        .active_probe_unit_for_ledger(&id.ledger_id)
                        .map_err(|error| {
                            tracing::error!(%error, job_id = %id.job_id, "读取待重试探测单元失败");
                        })?;
                if active
                    .as_ref()
                    .is_none_or(|unit| unit.job_id != id.job_id || unit.stage != id.stage)
                {
                    return Ok::<(), ()>(());
                }
                if active.as_ref().is_some_and(|unit| unit.status == "running") && !owns_running {
                    return Ok(());
                }
                store
                    .retry_probe_unit(&id.job_id, &id.ledger_id)
                    .map_err(|error| {
                        tracing::error!(%error, job_id = %id.job_id, "重排探测单元失败");
                    })?;
                seen.remove(&id.ledger_id);
                Ok(())
            }
        })
        .await;
        if matches!(result, Ok(Ok(()))) {
            manager.wake.notify_one();
            return;
        }
        if manager.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
fn cancel_deleted_dispatch(manager: &ProbeManager, id: &store::ProbeDispatchId) {
    let Some(store) = manager.dispatch_store() else {
        return;
    };
    let missing = match super::ledger_id(&id.ledger_id) {
        Some(file_id) => match store.file_coverage_by_id(file_id) {
            Ok(row) => row.is_none(),
            Err(error) => {
                tracing::error!(%error, ledger_id = %id.ledger_id, "检查待派发文件失败");
                return;
            }
        },
        None => false,
    };
    if !missing {
        return;
    }
    if let Err(error) = store.cancel_probe_unit(&id.job_id, &id.ledger_id, "file_deleted") {
        tracing::error!(%error, job_id = %id.job_id, ledger_id = %id.ledger_id, "删除文件后取消探测单元失败");
        return;
    }
    let refresh = store
        .get_probe_job(&id.job_id)
        .ok()
        .flatten()
        .is_some_and(|job| job.kind == "marker_refresh" && job.is_active());
    if refresh && let Err(error) = store.cancel_probe_job(&id.job_id, "file_deleted") {
        tracing::error!(%error, job_id = %id.job_id, "删除文件后取消刷新任务失败");
    }
    drop(store);
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
        self.0.wake.notify_one();
    }
}

/// Await the manager's wake signal, then refill the dispatch windows.
///
/// Every `wake.notify_waiters()` call site (window full, recovery, page admission) means
/// "durable ids may now fit again"; without a consumer those signals were dead code.
async fn run_window_dispatcher(manager: Arc<ProbeManager>) {
    loop {
        tokio::select! {
            _ = wait_for_shutdown(&manager) => break,
            _ = manager.wake.notified() => {},
        }
        if manager.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        if let Err(error) = step(&manager, |manager| manager.refill_drained_windows()).await {
            tracing::warn!(%error, "探测窗口补充失败，保留持久队列");
        }
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
            let _leases = match acquire_recovered_refresh_leases(&manager, &unit).await {
                Ok(leases) => leases,
                Err(reason) => {
                    if !manager.shutting_down.load(Ordering::SeqCst) {
                        let _ = step(&manager, {
                            let job_id = refreshed.id.clone();
                            move |manager| {
                                if let Err(error) =
                                    manager.store.lock().cancel_probe_job(&job_id, &reason)
                                {
                                    tracing::error!(%error, job_id, "取消恢复的刷新任务失败");
                                }
                            }
                        })
                        .await;
                    }
                    continue;
                }
            };
            wait_for_metadata_idle(&manager).await;
            let _ = step(&manager, move |manager| {
                super::marker_jobs::apply_marker_refresh(manager, &refreshed, &unit);
                super::timings::log_terminal_job(&manager.store, &manager.timings, &refreshed);
            })
            .await;
            continue;
        }

        let message = {
            let mut rx = manager.fingerprint_rx.lock().take();
            let message = match rx.as_mut() {
                Some(rx) => tokio::select! {
                    _ = wait_for_shutdown(&manager) => None,
                    message = rx.recv() => message,
                },
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

async fn acquire_recovered_refresh_leases(
    manager: &Arc<ProbeManager>,
    unit: &ProbeUnit,
) -> Result<Vec<crate::library_lifecycle::LibraryWorkLease>, String> {
    loop {
        let leased = step(manager, {
            let unit = unit.clone();
            move |manager| refresh_leases(manager, &unit)
        })
        .await;
        match leased {
            Ok(Ok(leases)) => return Ok(leases),
            Ok(Err(reason)) if !is_temporary_busy(&reason) => return Err(reason),
            Ok(Err(_)) => {}
            Err(error) => tracing::warn!(%error, "恢复刷新租约失败，稍后重试"),
        }
        if manager.shutting_down.load(Ordering::SeqCst) {
            return Err("probe worker shutting down".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
                Ok(Vec::new())
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
    let outcome = probe::probe_fingerprint_detailed(&manager, &work).await;
    let _ = step(&manager, move |manager| {
        if !source_ready(manager, &unit) {
            return;
        }
        match outcome {
            super::stages::FingerprintRunOutcome::Complete => {
                manager.finish_with_status(&unit, "succeeded", None, None);
            }
            super::stages::FingerprintRunOutcome::Partial { error_kind, error } => {
                manager.finish_with_status(&unit, "partial", Some(&error_kind), Some(&error));
            }
            super::stages::FingerprintRunOutcome::Failed { error_kind, error } => {
                manager.finish_with_status(&unit, "failed", Some(&error_kind), Some(&error));
            }
            super::stages::FingerprintRunOutcome::Cancelled { reason } => {
                manager.cancel_unit(&unit, &reason);
            }
        }
    })
    .await;
    owns_running
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
        .dispatch_store()
        .ok_or_else(|| "probe Store busy".to_string())?
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
        let store = manager
            .dispatch_store()
            .ok_or_else(|| "probe Store busy".to_string())?;
        crate::fingerprint_job::source::ensure_source_recorded(&store, &unit.row)?;
        unit.row.file.library_id.to_string()
    };
    registry
        .acquire_bounded(&manager.store, &library_id)
        .map(Some)
}

pub(super) fn source_ready(manager: &ProbeManager, unit: &ProbeUnit) -> bool {
    let Some(store) = manager.dispatch_store() else {
        return false;
    };
    if !unit_is_active(&store, unit) {
        drop(store);
        manager
            .seen
            .lock()
            .remove(&unit.row.file.file_id.to_string());
        return false;
    }
    let recorded = crate::fingerprint_job::source::ensure_source_recorded(&store, &unit.row);
    drop(store);
    let availability =
        recorded.and_then(|()| crate::fingerprint_job::source::observe_source(&unit.row));
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

/// Check the unit while holding the same Store guard used for publication.
pub(super) fn unit_is_active(store: &crate::Store, unit: &ProbeUnit) -> bool {
    let Some(job_id) = unit.job_id.as_deref() else {
        return true;
    };
    match store.is_probe_unit_active(job_id, &unit.row.file.file_id.to_string()) {
        Ok(active) => active,
        Err(error) => {
            tracing::error!(%error, job_id, file_id = %unit.row.file.file_id, "检查探测单元状态失败");
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
            || manager.shutting_down.load(Ordering::Acquire)
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
            || manager.shutting_down.load(Ordering::Acquire)
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
    let Some(store) = manager.dispatch_store() else {
        return false;
    };
    let started = match unit.job_id.as_deref() {
        Some(job_id) => store.start_probe_unit(job_id, &ledger_id),
        None => {
            tracing::error!(ledger_id, "队列单元缺少任务 ID，无法启动持久化任务");
            return false;
        }
    };
    drop(store);
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

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
