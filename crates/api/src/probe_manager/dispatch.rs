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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FingerprintDispatchError {
    Overflowed,
    Full,
    Closed,
}

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

pub(super) fn is_strm_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
}

impl ProbeManager {
    pub(super) fn should_defer_remote_dispatch(&self, path: &std::path::Path) -> bool {
        self.remote_reads_are_paused() && is_strm_path(path)
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
                let _paused = self.remote_reads_paused.lock();
                self.shutting_down.store(true, Ordering::SeqCst);
                self.remote_reads_wait_stopped
                    .store(true, Ordering::Release);
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
        let Some(mut receiver) = self.shutdown_drain.lock().clone() else {
            return Err("探测关闭状态缺失".into());
        };
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
        self.clear_paused_exempt_jobs();
        if self.metadata_pending.swap(0, Ordering::SeqCst) > 0 {
            self.metadata_idle_generation.fetch_add(1, Ordering::AcqRel);
            self.metadata_idle.notify_waiters();
        }
        let _workers = self.workers.lock();
        let paused = self.remote_reads_paused.lock();
        self.shutting_down.store(false, Ordering::SeqCst);
        self.remote_reads_wait_stopped
            .store(*paused, Ordering::Release);
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
            tracing::error!(ledger_id, "媒体信息已完成，但声纹任务缺少持久化 ID");
            self.remove_seen_any(&ledger_id);
            return false;
        };
        if !self.transition_to_fingerprint_stage(&current_job_id, &ledger_id, &mut work) {
            return false;
        }
        let Some(job_id) = work.unit.job_id.clone() else {
            return false;
        };
        self.set_seen(ledger_id.clone(), job_id.clone());
        let id = ProbeDispatchId {
            job_id: job_id.clone(),
            ledger_id: ledger_id.clone(),
            stage: ProbeDispatchStage::Fingerprint,
        };
        if (self.background_is_paused() && !self.is_dispatch_paused_exempt(&job_id, &ledger_id))
            || self.should_defer_remote_dispatch(&work.unit.row.file.path)
        {
            tracing::info!(
                ledger_id,
                "后台探测或远程读取已暂停，已完成的媒体信息按顺序留在数据库排队等待声纹"
            );
            self.remove_seen_if_job(&ledger_id, &job_id);
            return false;
        }
        match self.dispatch_fingerprint_work(id, work) {
            Ok(()) => true,
            Err(FingerprintDispatchError::Overflowed) => {
                tracing::info!(
                    ledger_id,
                    "声纹队列存在积压，已完成的媒体信息按顺序留在数据库排队"
                );
                self.remove_seen_if_job(&ledger_id, &job_id);
                self.wake.notify_waiters();
                false
            }
            Err(FingerprintDispatchError::Full) => {
                tracing::info!(
                    ledger_id,
                    "声纹窗口已满，已完成的媒体信息保持排队等待下一次派发"
                );
                self.remove_seen_if_job(&ledger_id, &job_id);
                self.wake.notify_waiters();
                false
            }
            Err(FingerprintDispatchError::Closed) => {
                tracing::info!(ledger_id, "声纹 worker 已停止，已完成的媒体信息保持排队");
                self.remove_seen_if_job(&ledger_id, &job_id);
                false
            }
        }
    }

    fn transition_to_fingerprint_stage(
        &self,
        current_job_id: &str,
        ledger_id: &str,
        work: &mut probe::FingerprintWork,
    ) -> bool {
        let store = self.store.as_ref();
        let current_job = match store.get_probe_job(current_job_id) {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::error!(ledger_id, job_id = %current_job_id, "媒体信息任务已不存在，无法转入声纹");
                self.remove_seen_if_job(ledger_id, current_job_id);
                return false;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id, "读取媒体信息任务失败，声纹保持排队");
                self.wake.notify_waiters();
                return false;
            }
        };
        let parent_job_exempt = self.is_job_paused_exempt(current_job_id);
        let parent_unit_exempt = self.is_unit_paused_exempt(current_job_id, ledger_id);
        let parent_promoted = self.was_unit_promoted(current_job_id, ledger_id);
        if current_job.kind == "media_probe" {
            let fingerprint_job_id = uuid::Uuid::new_v4().to_string();
            let transitioned = match store.complete_media_probe_and_enqueue_fingerprint(
                current_job_id,
                ledger_id,
                &fingerprint_job_id,
            ) {
                Ok(changed) => changed,
                Err(error) => {
                    tracing::error!(%error, ledger_id, "持久化媒体信息到声纹的交接失败");
                    false
                }
            };
            if !transitioned {
                if (parent_job_exempt || parent_unit_exempt || parent_promoted)
                    && self
                        .store
                        .get_probe_job(current_job_id)
                        .ok()
                        .flatten()
                        .is_some_and(|job| !job.is_active())
                {
                    self.unmark_job_paused_exempt(current_job_id);
                }
                return false;
            }
            self.unmark_unit_paused_exempt(current_job_id, ledger_id);
            self.unmark_unit_promoted(current_job_id, ledger_id);
            if parent_job_exempt {
                self.unmark_job_paused_exempt(current_job_id);
                self.mark_job_paused_exempt(fingerprint_job_id.clone());
            } else if parent_unit_exempt || parent_promoted {
                self.mark_unit_paused_exempt(fingerprint_job_id.clone(), ledger_id.to_string());
                self.mark_unit_promoted(fingerprint_job_id.clone(), ledger_id.to_string());
            }
            work.unit.job_id = Some(fingerprint_job_id);
            work.start_job = true;
            true
        } else if !self.advance_running_unit(current_job_id, ledger_id) {
            tracing::warn!(ledger_id, job_id = %current_job_id, "声纹阶段未持久化，保持原任务可恢复");
            self.wake.notify_waiters();
            false
        } else {
            work.start_job = true;
            true
        }
    }

    fn advance_running_unit(&self, job_id: &str, ledger_id: &str) -> bool {
        let store = self.store.as_ref();
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

    pub(super) fn dispatch_fingerprint_work(
        &self,
        id: ProbeDispatchId,
        work: probe::FingerprintWork,
    ) -> Result<(), FingerprintDispatchError> {
        if self.fingerprint_overflowed.load(Ordering::Acquire) {
            return Err(FingerprintDispatchError::Overflowed);
        }
        match self.fingerprint_tx.try_send((id, work)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.fingerprint_overflowed.store(true, Ordering::Release);
                Err(FingerprintDispatchError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(FingerprintDispatchError::Closed),
        }
    }

    pub(super) fn dispatch_metadata_id(&self, id: ProbeDispatchId) -> Result<(), ()> {
        if self.metadata_overflowed.load(Ordering::Acquire) {
            return Err(());
        }
        match self.publish_metadata_observed(&self.metadata_tx, id, || {}) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metadata_overflowed.store(true, Ordering::Release);
                Err(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
        }
    }

    fn offer(
        &self,
        tx: mpsc::Sender<ProbeDispatchId>,
        id: ProbeDispatchId,
        unit: ProbeUnit,
    ) -> Result<(), ProbeUnit> {
        if self.metadata_overflowed.load(Ordering::Acquire) {
            tracing::info!(
                ledger_id = %unit.row.file.file_id,
                "探测队列存在积压，新任务按顺序留在数据库排队"
            );
            return Err(unit);
        }
        match self.publish_metadata_observed(&tx, id, || {}) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metadata_overflowed.store(true, Ordering::Release);
                tracing::info!(
                    ledger_id = %unit.row.file.file_id,
                    "探测窗口已满，任务保持数据库排队"
                );
                Err(unit)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(unit),
        }
    }

    /// Drain unstarted queued items buffered in the in-memory channels without
    /// touching SQLite or interrupting any active read.
    pub(super) fn drain_buffered_dispatch(&self) {
        let metadata_drained = {
            let mut guard = self.metadata_rx.lock();
            let mut ids = Vec::new();
            if let Some(rx) = guard.as_mut() {
                while let Ok(id) = rx.try_recv() {
                    ids.push(id);
                }
            }
            ids
        };
        let fingerprint_drained = {
            let mut guard = self.fingerprint_rx.lock();
            let mut ids = Vec::new();
            if let Some(rx) = guard.as_mut() {
                while let Ok((id, _)) = rx.try_recv() {
                    ids.push(id);
                }
            }
            ids
        };
        if !metadata_drained.is_empty() || !fingerprint_drained.is_empty() {
            let mut seen = self.seen.lock();
            for id in &metadata_drained {
                super::remove_seen_entry_if_job(&mut seen, &id.ledger_id, &id.job_id);
            }
            for id in &fingerprint_drained {
                super::remove_seen_entry_if_job(&mut seen, &id.ledger_id, &id.job_id);
            }
        }
        for _ in &metadata_drained {
            self.metadata_stage_finished();
        }
        self.metadata_overflowed.store(false, Ordering::Release);
        self.fingerprint_overflowed.store(false, Ordering::Release);
    }

    /// Fill both windows from durable pending ids. Seen stays at window + active.
    pub(super) fn fill_dispatch_windows(&self) {
        if self.shutting_down.load(Ordering::SeqCst) || self.background_is_paused() {
            return;
        }
        self.fill_metadata();
        self.fill_fingerprint();
    }

    /// Top up a window once emptied or at half-drain when durable backlog overflowed.
    /// While paused, only explicit exempt jobs and units are refilled.
    pub(super) fn refill_drained_windows(&self) {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let paused = self.background_is_paused();
        if paused && !self.has_paused_exempt_work() {
            return;
        }
        let metadata_free = self.metadata_tx.capacity();
        if metadata_free == METADATA_WINDOW
            || (paused && metadata_free > 0)
            || (self.metadata_overflowed.load(Ordering::Acquire)
                && metadata_free >= METADATA_WINDOW / 2)
        {
            self.fill_metadata();
        }
        let fingerprint_free = self.fingerprint_tx.capacity();
        if fingerprint_free == FINGERPRINT_WINDOW
            || (paused && fingerprint_free > 0)
            || (self.fingerprint_overflowed.load(Ordering::Acquire)
                && fingerprint_free >= FINGERPRINT_WINDOW / 2)
        {
            self.fill_fingerprint();
        }
    }

    fn pending_stage_ids(
        &self,
        stage: ProbeDispatchStage,
        window: usize,
    ) -> Option<Vec<ProbeDispatchId>> {
        let store = self.store.as_ref();
        let remote_paused = self.remote_reads_are_paused();
        if self.background_is_paused() {
            let exempt_jobs = self.paused_exempt_job_ids();
            let exempt_units = self.paused_exempt_unit_pairs();
            if exempt_jobs.is_empty() && exempt_units.is_empty() {
                return None;
            }
            match store.library_read_snapshot(|conn| {
                conn.pending_probe_dispatch_ids_for_exemptions(
                    stage,
                    &exempt_jobs,
                    &exempt_units,
                    remote_paused,
                    window + 1,
                )
            }) {
                Ok(Ok((ids, active_jobs, active_units))) => {
                    self.prune_inactive_exempt_jobs(&exempt_jobs, &active_jobs);
                    self.prune_inactive_exempt_units(&exempt_units, &active_units);
                    return Some(ids);
                }
                Ok(Err(error)) | Err(error) => {
                    tracing::error!(%error, ?stage, "读取暂停豁免派发窗口失败");
                    return None;
                }
            }
        }
        let result = if remote_paused {
            store.pending_local_probe_dispatch_ids(stage, window + 1)
        } else {
            store.pending_probe_dispatch_ids(stage, window + 1)
        };
        match result {
            Ok(ids) => Some(ids),
            Err(error) => {
                tracing::error!(%error, ?stage, "读取派发窗口失败");
                None
            }
        }
    }

    fn fill_metadata(&self) {
        let tx = &self.metadata_tx;
        let mut free = tx.capacity();
        if free == 0 {
            return;
        }
        let Some(ids) = self.pending_stage_ids(ProbeDispatchStage::Metadata, METADATA_WINDOW)
        else {
            return;
        };

        let mut remaining_unseen = false;
        let mut seen = self.seen.lock();
        for id in ids {
            if seen.contains_key(&id.ledger_id) {
                continue;
            }
            if free == 0 {
                remaining_unseen = true;
                break;
            }
            seen.insert(id.ledger_id.clone(), id.job_id.clone());
            if self
                .publish_metadata_observed(tx, id.clone(), || {})
                .is_err()
            {
                super::remove_seen_entry_if_job(&mut seen, &id.ledger_id, &id.job_id);
                remaining_unseen = true;
                break;
            }
            free -= 1;
        }
        self.metadata_overflowed
            .store(remaining_unseen, Ordering::Release);
    }

    fn fill_fingerprint(&self) {
        let tx = &self.fingerprint_tx;
        let free = tx.capacity();
        if free == 0 {
            return;
        }
        let Some(ids) = self.pending_stage_ids(ProbeDispatchStage::Fingerprint, FINGERPRINT_WINDOW)
        else {
            return;
        };

        let mut ready = Vec::new();
        let mut remaining_unseen = false;
        for id in ids {
            if self.has_seen(&id.ledger_id) {
                continue;
            }
            if ready.len() >= free {
                remaining_unseen = true;
                break;
            }
            if let Some((_, work)) = self.load_dispatch_unit(&id) {
                ready.push((id, work));
            }
        }
        let mut seen = self.seen.lock();
        for (id, work) in ready {
            if seen.contains_key(&id.ledger_id) {
                continue;
            }
            seen.insert(id.ledger_id.clone(), id.job_id.clone());
            if tx.try_send((id.clone(), work)).is_err() {
                super::remove_seen_entry_if_job(&mut seen, &id.ledger_id, &id.job_id);
                remaining_unseen = true;
                break;
            }
        }
        self.fingerprint_overflowed
            .store(remaining_unseen, Ordering::Release);
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
        let store = self.store.as_ref();
        let snapshot = store.load_probe_dispatch_snapshot(id);

        let snapshot = match snapshot {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                self.remove_seen_if_job(&id.ledger_id, &id.job_id);
                return None;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id = %id.ledger_id, "读取探测派发快照失败");
                self.remove_seen_if_job(&id.ledger_id, &id.job_id);
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
            self.remove_seen_if_job(&id.ledger_id, &id.job_id);
            return None;
        }
        let kind = match snapshot.unit.kind.parse::<domain::MediaKind>() {
            Ok(kind) => kind,
            Err(_) => {
                tracing::error!(ledger_id = %id.ledger_id, kind = %snapshot.unit.kind, "派发单元媒体类型无效");
                let _ = self.store.finish_probe_unit(
                    &id.job_id,
                    &id.ledger_id,
                    false,
                    Some("派发时媒体类型无效"),
                );
                self.remove_seen_if_job(&id.ledger_id, &id.job_id);
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
                self.remove_seen_if_job(&id.ledger_id, &id.job_id);
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
        let store = self.store.as_ref();
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
