#[path = "worker/fingerprint.rs"]
mod fingerprint;
use fingerprint::run_fingerprint_worker;

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
            loop {
                let recovered = step(&bootstrap, |manager| {
                    manager.recovery_deferred.store(false, Ordering::SeqCst);
                    manager.recover_pending_jobs();
                    !manager.recovery_deferred.load(Ordering::SeqCst)
                })
                .await;
                if matches!(recovered, Ok(true)) {
                    break;
                }
                if let Err(error) = recovered {
                    tracing::error!(%error, "延迟恢复探测任务失败");
                    bootstrap.recovery_deferred.store(true, Ordering::SeqCst);
                }
                if bootstrap.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        let configured = step(&bootstrap, |manager| {
            manager
                .store
                .get_setting(crate::settings_keys::PROBE_CONCURRENCY)
                .ok()
                .flatten()
        })
        .await
        .map(|setting| metadata_worker_count(setting.as_deref()));
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

/// Default metadata probe workers when `probe.concurrency` is unset.
pub(crate) const DEFAULT_METADATA_WORKERS: usize = 1;
/// Upper bound for `probe.concurrency`.
pub(crate) const MAX_METADATA_WORKERS: usize = 8;
/// STRM sources allow exactly one read at a time.
pub(crate) const REMOTE_READ_LIMIT: usize = 1;
/// Pause after each remote read before the next one is admitted.
pub(crate) const REMOTE_READ_COOLDOWN: std::time::Duration = std::time::Duration::from_millis(500);

/// `probe.concurrency` → metadata worker count (default 1, clamped 1..=8).
pub(crate) fn metadata_worker_count(setting: Option<&str>) -> usize {
    setting
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_METADATA_WORKERS)
        .min(MAX_METADATA_WORKERS)
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[test]
    fn probe_concurrency_defaults_to_one_and_is_clamped() {
        assert_eq!(metadata_worker_count(None), 1);
        assert_eq!(metadata_worker_count(Some("0")), 1);
        assert_eq!(metadata_worker_count(Some("junk")), 1);
        assert_eq!(metadata_worker_count(Some(" 6 ")), 6);
        assert_eq!(metadata_worker_count(Some("99")), 8);
    }

    /// STRM sources allow one read at a time: the gate is pinned to one
    /// remote read with a 500ms cooldown, whatever probe.concurrency says.
    #[test]
    fn remote_reads_are_serial_with_a_500ms_cooldown() {
        assert_eq!(REMOTE_READ_LIMIT, 1);
        assert_eq!(REMOTE_READ_COOLDOWN, std::time::Duration::from_millis(500));
    }
}

fn start_workers(manager: &Arc<ProbeManager>, metadata_workers: usize) {
    // STRM limit: one remote read at a time across all workers, 500ms apart.
    marker::media_read::configure_remote_reads(REMOTE_READ_LIMIT, REMOTE_READ_COOLDOWN);
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
        move |manager| work_lease(manager, &unit, unit.marker_refresh_id.is_some())
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
    super::store_access::is_busy_message(reason)
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
                manager.store.library_write(|store| {
                    // Acquire the SQL scope first; never wait for seen while holding it.
                    let Some(mut seen) = manager.seen.try_lock() else {
                        return Err(store::StoreError::WriteBusy);
                    };
                    let active = store.active_probe_unit_for_ledger(&id.ledger_id).map_err(
                        |error| {
                            tracing::error!(%error, job_id = %id.job_id, "读取待重试探测单元失败");
                            error
                        },
                    )?;
                    if active
                        .as_ref()
                        .is_none_or(|unit| unit.job_id != id.job_id || unit.stage != id.stage)
                    {
                        return Ok(());
                    }
                    if active.as_ref().is_some_and(|unit| unit.status == "running") && !owns_running
                    {
                        return Ok(());
                    }
                    store
                        .retry_probe_unit(&id.job_id, &id.ledger_id)
                        .map_err(|error| {
                            tracing::error!(%error, job_id = %id.job_id, "重排探测单元失败");
                            error
                        })?;
                    seen.remove(&id.ledger_id);
                    Ok(())
                })
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
    let Some(file_id) = super::ledger_id(&id.ledger_id) else {
        return;
    };
    if !matches!(manager.store.file_coverage_by_id(file_id), Ok(None)) {
        return;
    }
    let cancelled = manager.store.library_write(|store| {
        if store.file_coverage_by_id(file_id)?.is_some() {
            return Ok(false);
        }
        store.cancel_probe_unit(&id.job_id, &id.ledger_id, "file_deleted")?;
        let refresh = store
            .get_probe_job(&id.job_id)?
            .is_some_and(|job| job.kind == "marker_refresh" && job.is_active());
        if refresh {
            store.cancel_probe_job(&id.job_id, "file_deleted")?;
        }
        Ok(true)
    });
    match cancelled {
        Ok(true) => {
            manager.seen.lock().remove(&id.ledger_id);
        }
        Ok(false) => {}
        Err(error) => {
            tracing::error!(%error, job_id=%id.job_id, ledger_id=%id.ledger_id, "删除文件后取消探测单元失败")
        }
    }
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
    let mut retry = tokio::time::interval(std::time::Duration::from_secs(1));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let recover_completed = tokio::select! {
            _ = wait_for_shutdown(&manager) => break,
            _ = manager.wake.notified() => false,
            _ = retry.tick() => true,
        };
        if manager.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        if let Err(error) = step(&manager, move |manager| {
            if recover_completed {
                manager.recover_completed_jobs();
            }
            manager.refill_drained_windows();
        })
        .await
        {
            tracing::warn!(%error, "探测窗口补充失败，保留持久队列");
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
        .file_coverage_for_media(unit.row.file.media_id)
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
    optional: bool,
) -> Result<Option<crate::library_lifecycle::LibraryWorkLease>, String> {
    let registry = manager.library_work.lock().clone();
    let Some(registry) = registry else {
        return Ok(None);
    };
    let library_id = {
        let store = manager.store.as_ref();
        crate::fingerprint_job::source::ensure_source_recorded(&store, &unit.row)?;
        unit.row.file.library_id.to_string()
    };
    let acquired = if optional {
        registry.acquire_optional(
            &manager.store,
            &library_id,
            super::tracks::optional_work_kind(unit),
        )
    } else {
        registry.acquire_bounded(&manager.store, &library_id)
    };
    acquired.map(Some).map_err(|error| error.to_string())
}

pub(super) fn source_ready(manager: &ProbeManager, unit: &ProbeUnit) -> bool {
    let recorded = manager.store.library_read_snapshot(|store| {
        if !unit_is_active(store, unit) {
            return None;
        }
        Some(crate::fingerprint_job::source::ensure_source_recorded_on(
            store, &unit.row,
        ))
    });
    let recorded = match recorded {
        Ok(Some(recorded)) => recorded,
        Ok(None) => {
            manager
                .seen
                .lock()
                .remove(&unit.row.file.file_id.to_string());
            return false;
        }
        Err(error) => Err(error.to_string()),
    };
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
pub(super) fn unit_is_active(store: &store::LibraryConnection<'_>, unit: &ProbeUnit) -> bool {
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
    let store = manager.store.as_ref();
    let started = match unit.job_id.as_deref() {
        Some(job_id) => store.start_probe_unit(job_id, &ledger_id),
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
            if !super::store_access::is_busy(&error) {
                manager.finish(unit, false);
            }
            false
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "worker_runtime_tests.rs"]
mod runtime_tests;
