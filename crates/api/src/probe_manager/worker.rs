#[path = "worker/fingerprint.rs"]
mod fingerprint;
use fingerprint::run_fingerprint_worker;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::library_lifecycle::BoundedLeaseError;

use super::{ProbeManager, ProbeUnit, probe};

pub(super) struct MetadataAdmission {
    active: usize,
    pub(super) limit: usize,
}

impl MetadataAdmission {
    pub(super) fn new(limit: usize) -> Self {
        Self { active: 0, limit }
    }
}

async fn next_metadata_work(
    manager: &Arc<ProbeManager>,
) -> Option<(store::ProbeDispatchId, MetadataPermit, MetadataStageGuard)> {
    // Keep exclusive reception until the queue head has admission. Otherwise
    // idle workers hide older ids while a completing worker takes a newer id
    // and races those waiters for the newly available slot.
    let _receiver = tokio::select! {
        _ = wait_for_shutdown(manager) => return None,
        guard = manager.metadata_receiver.lock() => guard,
    };
    let mut rx = manager.metadata_rx.lock().take()?;
    let id = tokio::select! {
        _ = wait_for_shutdown(manager) => return None,
        id = rx.recv() => id,
    };
    let id = id?;
    *manager.metadata_rx.lock() = Some(rx);
    let pending = MetadataStageGuard(manager.clone());
    let permit = metadata_permit(manager).await?;
    Some((id, permit, pending))
}

async fn metadata_permit(manager: &Arc<ProbeManager>) -> Option<MetadataPermit> {
    loop {
        let available = manager.metadata_capacity.notified();
        tokio::pin!(available);
        available.as_mut().enable();
        {
            let mut admission = manager.metadata_admission.lock();
            if admission.active < admission.limit {
                admission.active += 1;
                return Some(MetadataPermit(manager.clone()));
            }
        }
        tokio::select! {
            _ = wait_for_shutdown(manager) => return None,
            _ = available => {}
        }
    }
}

struct MetadataPermit(Arc<ProbeManager>);

impl Drop for MetadataPermit {
    fn drop(&mut self) {
        self.0.metadata_admission.lock().active -= 1;
        self.0.metadata_capacity.notify_waiters();
    }
}

pub(super) async fn wait_for_shutdown(manager: &ProbeManager) {
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
    try_start_observed(manager, || {})
}

pub(super) fn try_start_observed(
    manager: &Arc<ProbeManager>,
    after_settings_loaded: impl FnOnce(),
) -> bool {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("【媒体探测】当前不在 Tokio runtime 上下文，探测 worker 未启动");
        return false;
    }
    if manager.workers_started.load(Ordering::SeqCst) {
        manager.wake.notify_waiters();
        return true;
    }
    let settings_revision = manager.settings_revision();
    let (persisted_settings, persisted_remote_reads) = read_persisted_admission_settings(manager);
    after_settings_loaded();
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
    // Discard the persisted snapshot if an HTTP save was published while it loaded.
    // Both `remote_reads_paused` and `remote_reads_wait_stopped` are published
    // atomically inside `apply_startup_settings_if_unchanged`.
    manager.apply_startup_settings_if_unchanged(
        settings_revision,
        persisted_settings,
        persisted_remote_reads,
    );
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
        if bootstrap.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        start_workers(&bootstrap);
        if let Err(error) = step(&bootstrap, |manager| manager.fill_dispatch_windows()).await {
            tracing::error!(%error, "探测初始派发失败");
        }
    });
    workers.push(handle);
    true
}

fn read_persisted_admission_settings(
    manager: &ProbeManager,
) -> (
    Option<crate::probe_settings::ProbeSettings>,
    Option<crate::probe_settings::RemoteReadsSettings>,
) {
    let settings = crate::probe_settings::ProbeSettings::load(&manager.store)
        .map_err(
            |error| tracing::error!(%error, "启动探测 worker 时读取媒体信息设置失败，沿用内存设置"),
        )
        .ok();
    let remote_reads = crate::probe_settings::RemoteReadsSettings::load(&manager.store)
        .map_err(|error| {
            tracing::error!(%error, "启动探测 worker 时读取远程读取设置失败，先暂停远程读取")
        })
        .ok();
    (settings, remote_reads)
}

fn start_workers(manager: &Arc<ProbeManager>) {
    let mut workers = manager.workers.lock();
    if manager.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    workers.push(tokio::spawn(run_fingerprint_worker(manager.clone())));
    workers.push(tokio::spawn(super::analysis::run(manager.clone())));
    workers.push(tokio::spawn(run_window_dispatcher(manager.clone())));
    for _ in 0..crate::probe_settings::MAX_METADATA_WORKERS {
        let manager = manager.clone();
        workers.push(tokio::spawn(run_metadata_worker(manager)));
    }
    drop(workers);
    manager.wake.notify_waiters();
    tracing::info!(
        metadata_workers = manager.probe_settings.lock().metadata_workers,
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
        let Some((id, permit, pending)) = next_metadata_work(&manager).await else {
            break;
        };
        if manager.shutting_down.load(Ordering::SeqCst) {
            manager.seen.lock().remove(&id.ledger_id);
            continue;
        }
        let (handed_off, owns_running) = process_metadata(&manager, &id).await;
        drop(permit);
        if !handed_off {
            retry_dispatch(&manager, &id, owns_running).await;
        }
        let _ = step(&manager, |manager| manager.refill_drained_windows()).await;
        drop(pending);
    }
}

async fn finish_metadata_outcome(
    manager: &Arc<ProbeManager>,
    unit: &ProbeUnit,
    outcome: probe::MetadataOutcome,
) -> (bool, bool) {
    match outcome {
        probe::MetadataOutcome::Complete => {
            let _ = step(manager, {
                let unit = unit.clone();
                move |manager| manager.finish(&unit, true)
            })
            .await;
            (false, true)
        }
        probe::MetadataOutcome::Failed => {
            if manager.shutting_down.load(Ordering::Acquire)
                || marker::media_read::remote_reads_stopping()
            {
                return (false, true);
            }
            let _ = step(manager, {
                let unit = unit.clone();
                move |manager| manager.finish(&unit, false)
            })
            .await;
            (false, true)
        }
        probe::MetadataOutcome::Retry => (false, true),
        probe::MetadataOutcome::Fingerprint(work) => {
            let handed_off = step(manager, move |manager| {
                manager.enqueue_fingerprint_after_metadata(work)
            })
            .await
            .unwrap_or(false);
            (handed_off, true)
        }
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
    let lease = match leased {
        Ok(Ok(lease)) => lease,
        Ok(Err(reason)) => {
            if !is_temporary_busy(&reason) {
                let _ = step(&manager, {
                    let unit = unit.clone();
                    move |manager| manager.cancel_unit(&unit, &reason.to_string())
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
        move |manager| {
            source_ready(manager, &unit)
                && start_persisted_unit(manager, &unit, store::ProbeDispatchStage::Metadata)
        }
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
                        move |manager| manager.cancel_unit(&unit, &reason.to_string())
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
    let control = lease
        .as_ref()
        .map(|lease| lease.control().clone())
        .unwrap_or_default()
        .with_read_stop(manager.remote_reads_wait_stopped.clone());
    let outcome = probe::probe_metadata(&manager, &unit, &control).await;
    let still_ready = step(&manager, {
        let unit = unit.clone();
        move |manager| source_ready(manager, &unit)
    })
    .await
    .unwrap_or(false);
    if !still_ready {
        return (false, true);
    }
    finish_metadata_outcome(manager, &unit, outcome).await
}

fn is_temporary_busy(error: &BoundedLeaseError) -> bool {
    matches!(
        error,
        BoundedLeaseError::StoreBusy | BoundedLeaseError::RegistryBusy
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
) -> Result<Vec<crate::library_lifecycle::LibraryWorkLease>, BoundedLeaseError> {
    work_lease(manager, unit, true).map(|lease| lease.into_iter().collect())
}

pub(super) fn work_lease(
    manager: &ProbeManager,
    unit: &ProbeUnit,
    optional: bool,
) -> Result<Option<crate::library_lifecycle::LibraryWorkLease>, BoundedLeaseError> {
    let registry = manager.library_work.lock().clone();
    let Some(registry) = registry else {
        return Ok(None);
    };
    manager
        .store
        .library_read_snapshot(|store| {
            crate::fingerprint_job::source::ensure_source_recorded_on(store, &unit.row)
        })
        .map_err(|error| {
            if super::store_access::is_busy(&error) {
                BoundedLeaseError::StoreBusy
            } else {
                BoundedLeaseError::Store(error.to_string())
            }
        })?
        .map_err(BoundedLeaseError::Store)?;
    let library_id = unit.row.file.library_id.to_string();
    let acquired = if optional {
        registry.acquire_optional(
            &manager.store,
            &library_id,
            super::tracks::optional_work_kind(unit),
        )
    } else {
        registry.acquire_bounded(&manager.store, &library_id)
    };
    acquired.map(Some)
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
        Err(error) if super::store_access::is_busy(&error) => {
            tracing::warn!(%error, ledger_id = %unit.row.file.file_id, "检查探测源文件时读池繁忙，保留单元等待重试");
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

fn start_persisted_unit(
    manager: &ProbeManager,
    unit: &ProbeUnit,
    stage: store::ProbeDispatchStage,
) -> bool {
    let ledger_id = unit.row.file.file_id.to_string();
    let store = manager.store.as_ref();
    let started = match unit.job_id.as_deref() {
        Some(job_id) => store.start_probe_unit(job_id, &ledger_id, stage),
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
