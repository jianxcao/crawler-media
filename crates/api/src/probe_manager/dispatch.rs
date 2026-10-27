//! Bounded probe dispatch.
//!
//! Channels carry [`ProbeDispatchId`] only. The durable queue stays in the
//! store; workers load one snapshot, claim it, and refill the window. A full
//! fingerprint window leaves the unit queued instead of finishing it failed.

use std::sync::atomic::Ordering;

use store::{ProbeDispatchId, ProbeDispatchStage};
use tokio::sync::mpsc;

use super::{ProbeManager, ProbeUnit, probe};
use crate::scrape_store::ScrapeStoreExt;

pub(super) const METADATA_WINDOW: usize = 64;
pub(super) const FINGERPRINT_WINDOW: usize = 16;

pub(super) struct DispatchChannels {
    pub(super) metadata_tx: mpsc::Sender<ProbeDispatchId>,
    pub(super) metadata_rx: mpsc::Receiver<ProbeDispatchId>,
    pub(super) fingerprint_tx: mpsc::Sender<(ProbeDispatchId, probe::FingerprintWork)>,
    pub(super) fingerprint_rx: mpsc::Receiver<(ProbeDispatchId, probe::FingerprintWork)>,
}

pub(super) fn channels() -> DispatchChannels {
    let (metadata_tx, metadata_rx) = mpsc::channel(METADATA_WINDOW);
    let (fingerprint_tx, fingerprint_rx) = mpsc::channel(FINGERPRINT_WINDOW);
    DispatchChannels {
        metadata_tx,
        metadata_rx,
        fingerprint_tx,
        fingerprint_rx,
    }
}

impl ProbeManager {
    pub(super) fn dispatch_store(&self) -> Option<parking_lot::MutexGuard<'_, store::Store>> {
        let guard = self
            .store
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT);
        if guard.is_none() {
            tracing::warn!("探测 Store 等待预算耗尽，保留持久任务等待重试");
        }
        guard
    }

    /// How many ledger ids are queued or running in this process.
    pub fn queued_ledger_count(&self) -> usize {
        self.seen.lock().len()
    }

    pub fn worker_count(&self) -> usize {
        self.workers.lock().len()
    }

    pub(super) async fn shutdown_dispatch(&self) -> Result<(), String> {
        let _shutdown = self.shutdown_lock.lock().await;
        if self.shutdown_drain.lock().is_none() {
            let handles = {
                let mut workers = self.workers.lock();
                if !self.workers_started.swap(false, Ordering::SeqCst) {
                    return Ok(());
                }
                self.shutting_down.store(true, Ordering::SeqCst);
                self.shutdown_signal.send_replace(true);
                std::mem::take(&mut *workers)
            };
            self.wake.notify_waiters();
            self.metadata_idle.notify_waiters();
            drop(self.metadata_rx.lock().take());
            drop(self.fingerprint_rx.lock().take());
            let (finished, receiver) = tokio::sync::watch::channel(false);
            *self.shutdown_drain.lock() = Some(receiver);
            let lease = self.store_lease.lock().take();
            let (drained, wait_for_drain) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                if wait_for_drain.recv().is_err() {
                    // The runtime disappeared while work might still be blocking.
                    // Keep ownership rather than let a new manager overlap it.
                    tracing::error!("探测 runtime 在排空前停止，保留数据库探测租约直到进程退出");
                    std::mem::forget(lease);
                    return;
                }
                drop(lease);
                finished.send_replace(true);
            });
            tokio::spawn(async move {
                for handle in handles {
                    if let Err(error) = handle.await {
                        tracing::error!(%error, "探测 worker 关闭时异常退出");
                    }
                }
                let _ = drained.send(());
            });
        }
        let mut receiver = self.shutdown_drain.lock().as_ref().unwrap().clone();
        let finished = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
        if finished.is_err() || !*receiver.borrow() {
            tracing::warn!("探测 worker 关闭超时，后台继续排空并保留数据库探测租约");
            return Err("探测 worker 关闭超时，旧任务仍在排空".into());
        }
        self.shutdown_drain.lock().take();
        self.seen.lock().clear();
        if self.metadata_pending.swap(0, Ordering::SeqCst) > 0 {
            self.metadata_idle_generation.fetch_add(1, Ordering::AcqRel);
            self.metadata_idle.notify_waiters();
        }
        let _workers = self.workers.lock();
        self.shutting_down.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Queue one metadata id. A full window leaves the unit durable and queued.
    pub(super) fn queue_metadata(&self, unit: ProbeUnit) -> Result<(), ProbeUnit> {
        let Some(job_id) = unit.job_id.clone() else {
            return Err(unit);
        };
        let id = ProbeDispatchId {
            job_id,
            ledger_id: unit.row.file.file_id.to_string(),
            stage: ProbeDispatchStage::Metadata,
        };
        self.offer(self.metadata_tx.clone(), id, unit)
    }

    /// Persist the metadata result as fingerprint work, then offer its id.
    ///
    /// A full or closed fingerprint window keeps the unit `queued` and wakes
    /// the dispatcher. It does not finish the unit failed.
    pub(super) fn enqueue_fingerprint_after_metadata(
        &self,
        mut work: probe::FingerprintWork,
    ) -> bool {
        let ledger_id = work.unit.row.file.file_id.to_string();
        let Some(current_job_id) = work.unit.job_id.clone() else {
            tracing::error!(ledger_id, "媒体信息已完成，但声纹任务缺少持久化任务 ID");
            self.seen.lock().remove(&ledger_id);
            return false;
        };
        let Some(store) = self.dispatch_store() else {
            return false;
        };
        let current_job = store.get_probe_job(&current_job_id);
        drop(store);
        let current_job = match current_job {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::error!(ledger_id, job_id = %current_job_id, "媒体信息任务已不存在，无法转入声纹");
                self.seen.lock().remove(&ledger_id);
                return false;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id, "读取媒体信息任务失败，声纹保持排队");
                self.wake.notify_waiters();
                return false;
            }
        };

        if current_job.kind == "media_probe" {
            let fingerprint_job_id = uuid::Uuid::new_v4().to_string();
            let Some(store) = self.dispatch_store() else {
                return false;
            };
            let transitioned = match store.complete_media_probe_and_enqueue_fingerprint(
                &current_job_id,
                &ledger_id,
                &fingerprint_job_id,
            ) {
                Ok(changed) => changed,
                Err(error) => {
                    tracing::error!(%error, ledger_id, "持久化媒体信息到声纹的交接失败");
                    false
                }
            };
            drop(store);
            if !transitioned {
                return false;
            }
            work.unit.job_id = Some(fingerprint_job_id);
            work.start_job = true;
        } else if !self.advance_running_unit(&current_job_id, &ledger_id) {
            tracing::warn!(ledger_id, job_id = %current_job_id, "声纹阶段未持久化，保持原任务可恢复");
            self.wake.notify_waiters();
            return false;
        } else {
            work.start_job = true;
        }

        let Some(job_id) = work.unit.job_id.clone() else {
            return false;
        };
        let id = ProbeDispatchId {
            job_id,
            ledger_id: ledger_id.clone(),
            stage: ProbeDispatchStage::Fingerprint,
        };
        match self.fingerprint_tx.try_send((id, work)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::info!(
                    ledger_id,
                    "声纹窗口已满，已完成的媒体信息保持排队等待下一次派发"
                );
                self.seen.lock().remove(&ledger_id);
                self.wake.notify_waiters();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::info!(ledger_id, "声纹 worker 已停止，已完成的媒体信息保持排队");
                self.seen.lock().remove(&ledger_id);
                false
            }
        }
    }

    fn advance_running_unit(&self, job_id: &str, ledger_id: &str) -> bool {
        let Some(store) = self.dispatch_store() else {
            return false;
        };
        match store.advance_probe_unit_to_fingerprint(job_id, ledger_id) {
            Ok(true) => true,
            Ok(false) => false,
            Err(error) => {
                tracing::error!(%error, job_id, ledger_id, "持久化声纹阶段失败");
                false
            }
        }
    }

    fn publish_metadata_observed(
        &self,
        tx: &mpsc::Sender<ProbeDispatchId>,
        id: ProbeDispatchId,
        observed: impl FnOnce(),
    ) -> Result<(), mpsc::error::TrySendError<()>> {
        let permit = tx.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => mpsc::error::TrySendError::Full(()),
            mpsc::error::TrySendError::Closed(_) => mpsc::error::TrySendError::Closed(()),
        })?;
        self.metadata_pending.fetch_add(1, Ordering::AcqRel);
        permit.send(id);
        observed();
        Ok(())
    }

    fn offer(
        &self,
        tx: mpsc::Sender<ProbeDispatchId>,
        id: ProbeDispatchId,
        unit: ProbeUnit,
    ) -> Result<(), ProbeUnit> {
        match self.publish_metadata_observed(&tx, id, || {}) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::info!(
                    ledger_id = %unit.row.file.file_id,
                    "探测窗口已满，任务保持数据库排队"
                );
                Err(unit)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(unit),
        }
    }

    /// Fill both windows from durable pending ids. Seen stays at window + active.
    pub(super) fn fill_dispatch_windows(&self) {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        self.fill_metadata();
        self.fill_fingerprint();
    }

    /// Top up a window only once it is fully drained.
    ///
    /// Every refill runs one `pending_probe_dispatch_ids` query over the queued set, so
    /// refilling after *each* completed unit turns a drain of N units into N queries. The
    /// natural batching point is an empty window: that is exactly when the worker would
    /// otherwise block, and a full `WINDOW` batch of ids is offered in a single query.
    pub(super) fn refill_drained_windows(&self) {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        if self.metadata_tx.capacity() == METADATA_WINDOW {
            self.fill_metadata();
        }
        if self.fingerprint_tx.capacity() == FINGERPRINT_WINDOW {
            self.fill_fingerprint();
        }
    }

    fn fill_metadata(&self) {
        let tx = &self.metadata_tx;
        let free = tx.capacity();
        if free == 0 {
            return;
        }
        let Some(store) = self.dispatch_store() else {
            return;
        };
        let ids = match store.pending_probe_dispatch_ids(ProbeDispatchStage::Metadata, free) {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(%error, "读取媒体信息派发窗口失败");
                return;
            }
        };
        drop(store);
        let mut seen = self.seen.lock();
        for id in ids {
            if !seen.insert(id.ledger_id.clone()) {
                continue;
            }
            if self
                .publish_metadata_observed(tx, id.clone(), || {})
                .is_err()
            {
                seen.remove(&id.ledger_id);
                break;
            }
        }
    }

    fn fill_fingerprint(&self) {
        let tx = &self.fingerprint_tx;
        let free = tx.capacity();
        if free == 0 {
            return;
        }
        let Some(store) = self.dispatch_store() else {
            return;
        };
        let ids = match store.pending_probe_dispatch_ids(ProbeDispatchStage::Fingerprint, free) {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(%error, "读取声纹派发窗口失败");
                return;
            }
        };
        drop(store);
        let mut ready = Vec::new();
        for id in ids {
            if self.seen.lock().contains(&id.ledger_id) {
                continue;
            }
            if let Some((_, work)) = self.load_dispatch_unit(&id) {
                ready.push((id, work));
            }
        }
        let mut seen = self.seen.lock();
        for (id, work) in ready {
            if !seen.insert(id.ledger_id.clone()) {
                continue;
            }
            if tx.try_send((id.clone(), work)).is_err() {
                seen.remove(&id.ledger_id);
                break;
            }
        }
    }

    pub(super) fn load_dispatch_unit(
        &self,
        id: &ProbeDispatchId,
    ) -> Option<(ProbeUnit, probe::FingerprintWork)> {
        self.materialize_dispatch(id, true)
    }

    #[cfg(test)]
    pub(super) fn load_dispatch_unit_inner(
        &self,
        id: &ProbeDispatchId,
        queued_only: bool,
    ) -> Option<(ProbeUnit, probe::FingerprintWork)> {
        self.materialize_dispatch(id, queued_only)
    }

    fn materialize_dispatch(
        &self,
        id: &ProbeDispatchId,
        queued_only: bool,
    ) -> Option<(ProbeUnit, probe::FingerprintWork)> {
        let Some(store) = self.dispatch_store() else {
            self.seen.lock().remove(&id.ledger_id);
            self.wake.notify_one();
            return None;
        };
        let snapshot = store.load_probe_dispatch_snapshot(id);
        drop(store);
        let snapshot = match snapshot {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                self.seen.lock().remove(&id.ledger_id);
                return None;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id = %id.ledger_id, "读取探测派发快照失败");
                self.seen.lock().remove(&id.ledger_id);
                self.wake.notify_waiters();
                return None;
            }
        };
        let acceptable = if queued_only {
            snapshot.unit.status == "queued"
        } else {
            snapshot.unit.status == "queued" || snapshot.unit.status == "running"
        };
        if snapshot.unit.stage != id.stage || !acceptable {
            self.seen.lock().remove(&id.ledger_id);
            return None;
        }
        let kind = match snapshot.unit.kind.parse::<domain::MediaKind>() {
            Ok(kind) => kind,
            Err(_) => {
                tracing::error!(ledger_id = %id.ledger_id, kind = %snapshot.unit.kind, "派发单元媒体类型无效");
                let _ = self.dispatch_store()?.finish_probe_unit(
                    &id.job_id,
                    &id.ledger_id,
                    false,
                    Some("派发时媒体类型无效"),
                );
                self.seen.lock().remove(&id.ledger_id);
                return None;
            }
        };
        let unit = ProbeUnit {
            row: snapshot.row,
            kind,
            force_fingerprint: snapshot.unit.force_fingerprint,
            reuse_fingerprint_cache: snapshot.unit.reuse_fingerprint_cache,
            overwrite_markers: snapshot.unit.overwrite_markers,
            reuse_media_info_cache: snapshot.unit.reuse_media_info_cache,
            marker_refresh_id: (snapshot.job.kind == "marker_refresh").then(|| {
                self.next_marker_refresh_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }),
            job_id: Some(snapshot.job.id),
        };
        let (media_duration_ms, sample_duration_secs) = match self
            .dispatch_timings(&id.ledger_id, &snapshot.source_version)
        {
            Ok(settings) => settings,
            Err(error) => {
                tracing::error!(%error, ledger_id = %id.ledger_id, "恢复派发单元的时长或声纹配置失败，保留持久任务");
                self.seen.lock().remove(&id.ledger_id);
                return None;
            }
        };
        let work = probe::FingerprintWork {
            unit: unit.clone(),
            media_duration_ms,
            source_version: snapshot.source_version,
            duration_secs: sample_duration_secs,
            start_job: true,
        };
        Some((unit, work))
    }

    /// The sampling configuration a recovered fingerprint unit must reuse.
    ///
    /// A hardcoded window would sample the wrong span and invalidate the cache key, so a
    /// unit refilled from the durable backlog must derive the same `(media_duration_ms,
    /// sample_duration_secs)` pair the original admission used: the cached duration only
    /// counts while it was measured for this exact source version.
    fn dispatch_timings(
        &self,
        ledger_id: &str,
        source_version: &str,
    ) -> Result<(Option<i64>, u32), store::StoreError> {
        let store = self.dispatch_store().ok_or_else(|| {
            store::StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "probe Store busy",
            ))
        })?;
        let media_duration_ms = store
            .get_media_info_cache_version(ledger_id)?
            .filter(|version| version.source_version == source_version)
            .and_then(|version| version.format_duration_ms);
        let sample_duration_secs = store
            .get_scrape_config()?
            .effective
            .fingerprint_duration_secs;
        Ok((media_duration_ms, sample_duration_secs))
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
