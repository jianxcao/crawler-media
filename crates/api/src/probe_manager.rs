//! 异步媒体探测队列（ProbeManager）。
//!
//! 详情页 / Jellyfin 请求**绝不等待探测**：未缓存的文件立即入队，由后台
//! worker 先提取并持久化媒体流信息，再将可选的片头片尾声纹投递到后台队列。
//! 详情页 / Jellyfin 请求不等待探测，结果写入数据库供前端轮询。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use domain::{LedgerRow, MediaId, MediaKind};
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc};

use marker::fingerprint::capture_types::FingerprintCaptureEngine;
use marker::{ChromaprintEngine, FingerprintEngine};

use crate::scrape_store::ScrapeStoreExt;
use crate::Store;

mod marker_jobs;
mod markers;
pub mod policy;
mod probe;
pub mod progress;
mod queue;
pub mod recovery;
pub mod season;
pub mod stages;
pub(crate) mod timings;
mod worker;

/// 一个待探测单元（对应一条 ledger 行）。
#[derive(Clone)]
pub struct ProbeUnit {
    pub row: LedgerRow,
    pub kind: MediaKind,
    pub force_fingerprint: bool,
    pub reuse_fingerprint_cache: bool,
    pub overwrite_markers: bool,
    pub reuse_media_info_cache: bool,
    pub marker_refresh_id: Option<u64>,
    /// Persisted task identity carried with the queue message. This prevents
    /// stale messages from attaching to a later retry for the same ledger row.
    pub job_id: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ProbeEnqueueError {
    AlreadyRunning,
    Persistence,
}

pub struct ProbeManager {
    store: Arc<Mutex<Store>>,
    fingerprint_engine: Arc<dyn FingerprintEngine>,
    capture_engine: Arc<dyn FingerprintCaptureEngine>,
    metadata_tx: mpsc::UnboundedSender<ProbeUnit>,
    fingerprint_tx: mpsc::UnboundedSender<probe::FingerprintWork>,
    /// 幂等去重：正在排队或执行中的 ledger_id。
    seen: Mutex<HashSet<String>>,
    last_failure: Mutex<HashMap<String, Instant>>,
    next_marker_refresh_id: AtomicU64,
    pub(crate) metadata_pending: Arc<AtomicUsize>,
    pub(crate) metadata_idle: Arc<Notify>,
    pub(crate) timings: timings::ProbeTimingLedger,
    /// Metadata workers share the high-priority queue; voiceprint has its own worker.
    metadata_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ProbeUnit>>>,
    fingerprint_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<probe::FingerprintWork>>>,
    pub(crate) recovered_marker_refreshes: Mutex<Vec<(crate::store::ProbeJob, ProbeUnit)>>,
}

impl ProbeManager {
    pub fn new(store: Arc<Mutex<Store>>) -> Self {
        Self::with_engines(
            store,
            Arc::new(ChromaprintEngine),
            Arc::new(ChromaprintEngine),
        )
    }

    pub fn with_fingerprint_engine(
        store: Arc<Mutex<Store>>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
    ) -> Self {
        Self::with_engines(store, fingerprint_engine, Arc::new(ChromaprintEngine))
    }

    pub fn with_engines(
        store: Arc<Mutex<Store>>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
    ) -> Self {
        let (metadata_tx, metadata_rx) = mpsc::unbounded_channel();
        let (fingerprint_tx, fingerprint_rx) = mpsc::unbounded_channel();
        let manager = Self {
            store,
            fingerprint_engine,
            capture_engine,
            metadata_tx,
            fingerprint_tx,
            seen: Mutex::new(HashSet::new()),
            last_failure: Mutex::new(HashMap::new()),
            next_marker_refresh_id: AtomicU64::new(1),
            metadata_pending: Arc::new(AtomicUsize::new(0)),
            metadata_idle: Arc::new(Notify::new()),
            timings: timings::ProbeTimingLedger::default(),
            metadata_rx: Arc::new(tokio::sync::Mutex::new(metadata_rx)),
            fingerprint_rx: Arc::new(tokio::sync::Mutex::new(fingerprint_rx)),
            recovered_marker_refreshes: Mutex::new(Vec::new()),
        };
        manager.recover_pending_jobs();
        manager
    }

    pub fn priority_gate(&self) -> Arc<dyn crate::fingerprint_job::CaptureGate> {
        Arc::new(progress::MetadataPriorityGate::from_manager(self))
    }

    pub fn request_probe(
        &self,
        row: &domain::LedgerRow,
        origin: policy::ProbeRequestOrigin,
    ) -> Result<policy::ProbeRequestResult, store::StoreError> {
        let store = self.store.lock();
        let ledger_id = row.id.to_string();
        let kind = store
            .get_media(row.media_id)
            .ok()
            .flatten()
            .map(|m| m.kind)
            .unwrap_or(domain::MediaKind::Movie);

        let path = std::path::Path::new(&row.path);
        let fp_enabled = policy::is_fingerprint_enabled_for_path(&store, path, kind);
        let marker_enabled = policy::is_marker_detection_enabled_for_path(&store, path, kind);

        if !marker_enabled && kind == domain::MediaKind::Tv && origin != policy::ProbeRequestOrigin::ManualRefresh {
            return Ok(policy::ProbeRequestResult::Disabled);
        }

        let source_version = crate::fingerprint_job::current_source_version(path);
        let sample_duration_secs = store
            .get_scrape_config()
            .ok()
            .map(|c| c.effective.fingerprint_duration_secs)
            .unwrap_or(180);

        let cached_meta_ver = store.get_media_info_cache_version(&ledger_id).ok().flatten();
        let meta_valid = cached_meta_ver
            .as_ref()
            .is_some_and(|v| v.source_version == source_version && v.format_duration_ms.is_some_and(|d| d > 0));

        let media_duration_ms = cached_meta_ver.and_then(|v| v.format_duration_ms);
        let outro_expected = media_duration_ms
            .filter(|d| *d > 0)
            .is_some_and(|d| d / 1000 > i64::from(sample_duration_secs.saturating_add(30)));

        let expected_fp_key = crate::fingerprint_job::fingerprint_cache_key(
            &source_version,
            sample_duration_secs,
            media_duration_ms,
        );

        let cached_fp = store.get_fingerprint_cache(&ledger_id).ok().flatten();
        let intro_missing = fp_enabled && cached_fp.as_ref().map_or(true, |c| {
            c.cache_key != expected_fp_key || c.intro.is_empty()
        });
        let outro_missing = fp_enabled && outro_expected && cached_fp.as_ref().map_or(true, |c| {
            c.cache_key != expected_fp_key || c.outro.as_ref().map_or(true, |o| o.is_empty())
        });

        // Check if there are active probe jobs for this ledger
        if let Ok(Some(_active_unit)) = store.active_probe_unit_for_ledger(&ledger_id) {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }

        // Check stage state retries if not manual
        let now_ms = now_ms();
        if origin != policy::ProbeRequestOrigin::ManualRetry && origin != policy::ProbeRequestOrigin::ManualRefresh {
            if fp_enabled && outro_missing && !intro_missing {
                let outro_key = store::ProbeStageKey {
                    ledger_id: ledger_id.clone(),
                    context_key: expected_fp_key.clone(),
                    stage: store::ProbeStage::Outro,
                };
                if let Ok(Some(stage_state)) = store.get_probe_stage(&outro_key) {
                    if stage_state.status == store::ProbeStageStatus::Failed {
                        if stage_state.failure_count >= 5 {
                            return Ok(policy::ProbeRequestResult::Exhausted);
                        }
                        if let Some(next_retry) = stage_state.next_retry_at_ms {
                            if next_retry > now_ms {
                                return Ok(policy::ProbeRequestResult::Waiting {
                                    next_retry_at_ms: next_retry,
                                });
                            }
                        }
                    }
                }
            }
        }

        drop(store);

        if meta_valid && !intro_missing && !outro_missing {
            return Ok(policy::ProbeRequestResult::Complete);
        }

        let unit = ProbeUnit {
            row: row.clone(),
            kind,
            force_fingerprint: fp_enabled,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        };

        if meta_valid && fp_enabled && (intro_missing || outro_missing) {
            // Directly queue fingerprint probe without media probe!
            if let Some(job_id) = self.enqueue_direct_fingerprint(unit, media_duration_ms, source_version, sample_duration_secs) {
                return Ok(policy::ProbeRequestResult::Queued { job_id });
            } else {
                return Ok(policy::ProbeRequestResult::AlreadyRunning);
            }
        }

        let ledger_id = unit.row.id.to_string();
        if self.enqueue(unit) {
            let job_id = self
                .active_probe_job_id(&ledger_id)
                .unwrap_or_default();
            Ok(policy::ProbeRequestResult::Queued { job_id })
        } else {
            Ok(policy::ProbeRequestResult::AlreadyRunning)
        }
    }

    pub fn enqueue_direct_fingerprint(
        &self,
        mut unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
    ) -> Option<String> {
        let ledger_id = unit.row.id.to_string();
        let mut seen = self.seen.lock();
        if !seen.insert(ledger_id.clone()) {
            return None;
        }
        let job_id = self.persist_single_unit(&unit, "fingerprint_probe")?;
        unit.job_id = Some(job_id.clone());

        let work = probe::FingerprintWork {
            unit: unit.clone(),
            media_duration_ms,
            source_version,
            duration_secs,
            start_job: true,
        };

        if self.fingerprint_tx.send(work).is_err() {
            seen.remove(&ledger_id);
            self.finish(&unit, false);
            return None;
        }
        Some(job_id)
    }

    /// 入队探测（幂等：已在队列/执行中的跳过）。
    pub fn enqueue(&self, mut unit: ProbeUnit) -> bool {
        unit.reuse_fingerprint_cache = true;
        unit.reuse_media_info_cache = true;
        let ledger_id = unit.row.id.to_string();
        let mut seen = self.seen.lock();
        if !seen.insert(ledger_id.clone()) {
            return false;
        }
        if self
            .last_failure
            .lock()
            .get(&ledger_id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            seen.remove(&ledger_id);
            return false;
        }
        self.last_failure.lock().remove(&ledger_id);
        let Some(job_id) = self.persist_single_unit(&unit, "media_probe") else {
            seen.remove(&ledger_id);
            return false;
        };
        unit.job_id = Some(job_id.clone());
        tracing::info!(
            path = %unit.row.path,
            job_id,
            "【媒体信息】已进入高优先级探测队列；声纹将在媒体信息写入后另行排队"
        );
        if self.queue_metadata(unit.clone()).is_err() {
            seen.remove(&ledger_id);
            drop(seen);
            self.finish(&unit, false);
            return false;
        }
        true
    }

    /// 强制探测：同一文件已有任务时拒绝重复排队；终态后用户可重试。
    pub fn enqueue_force(&self, mut unit: ProbeUnit) -> bool {
        unit.reuse_fingerprint_cache = false;
        unit.reuse_media_info_cache = false;
        let ledger_id = unit.row.id.to_string();
        let mut seen = self.seen.lock();
        self.last_failure.lock().remove(&ledger_id);
        if !seen.insert(ledger_id.clone()) {
            tracing::info!(ledger_id, "已有媒体探测任务，跳过重复强制探测");
            return false;
        }
        let Some(job_id) = self.persist_single_unit(&unit, "media_probe") else {
            seen.remove(&ledger_id);
            return false;
        };
        unit.job_id = Some(job_id.clone());
        if self.queue_metadata(unit.clone()).is_err() {
            seen.remove(&ledger_id);
            drop(seen);
            self.finish(&unit, false);
            return false;
        }
        true
    }

    /// Force a complete season refresh while leaving its current markers and
    /// chapter caches visible. Results replace the old markers only after all
    /// episodes finish successfully.
    pub fn enqueue_marker_refresh(&self, mut units: Vec<ProbeUnit>) -> usize {
        let Some(first) = units.first() else {
            return 0;
        };
        let media_id = first.row.media_id;
        let season = first.row.season.unwrap_or(1);
        if units
            .iter()
            .any(|unit| unit.row.media_id != media_id || unit.row.season.unwrap_or(1) != season)
        {
            tracing::error!(%media_id, season, "拒绝创建跨媒体或跨季的片头片尾刷新批次");
            return 0;
        }

        let id = self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed);
        let episode_count = units.len();
        let started_at = Instant::now();
        let job_id = uuid::Uuid::new_v4().to_string();
        let scope = marker_refresh_scope(media_id, season);
        let ledger_ids = units
            .iter()
            .map(|unit| unit.row.id.to_string())
            .collect::<Vec<_>>();
        let specs = units
            .iter()
            .zip(&ledger_ids)
            .map(|(unit, ledger_id)| crate::store::ProbeJobUnitSpec {
                ledger_id,
                kind: unit.kind.as_str(),
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            })
            .collect::<Vec<_>>();
        match self.store.lock().create_probe_job(
            &job_id,
            "marker_refresh",
            &media_id.to_string(),
            Some(season),
            &scope,
            &specs,
        ) {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(%media_id, season, "该剧该季已有片头片尾刷新任务，拒绝重复入队");
                return 0;
            }
            Err(error) => {
                tracing::error!(%error, %media_id, season, "创建持久化片头片尾刷新任务失败");
                return 0;
            }
        }

        let mut queued = 0;
        for unit in &mut units {
            unit.force_fingerprint = true;
            unit.reuse_fingerprint_cache = false;
            unit.reuse_media_info_cache = true;
            unit.overwrite_markers = true;
            unit.marker_refresh_id = Some(id);
            unit.job_id = Some(job_id.clone());
            self.seen.lock().insert(unit.row.id.to_string());
            if self.queue_metadata(unit.clone()).is_ok() {
                queued += 1;
            } else {
                self.finish(unit, false);
            }
        }
        tracing::info!(%media_id, season, refresh_id = id, job_id, episodes = episode_count, queued,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【片头片尾】已启动强制刷新，旧标记保持可见直到整季探测完成");
        queued
    }

    /// Reserve every file for a manual item refresh in one database transaction.
    /// For TV, retain old markers until all files finish and compare each season once.
    pub(crate) fn enqueue_forced_item_refresh(
        &self,
        mut units: Vec<ProbeUnit>,
    ) -> Result<usize, ProbeEnqueueError> {
        let Some(first) = units.first() else {
            return Ok(0);
        };
        let media_id = first.row.media_id;
        let kind = first.kind;
        if units
            .iter()
            .any(|unit| unit.row.media_id != media_id || unit.kind != kind)
        {
            tracing::error!(%media_id, "拒绝创建跨媒体或媒体类型不一致的手动探测任务");
            return Err(ProbeEnqueueError::Persistence);
        }

        let is_tv = kind == MediaKind::Tv;
        let job_kind = if is_tv {
            "marker_refresh"
        } else {
            "media_probe"
        };
        let scope = if is_tv {
            format!("marker-refresh:{media_id}:all")
        } else {
            format!("manual-probe:{media_id}")
        };
        let seasons = units
            .iter()
            .map(|unit| unit.row.season.unwrap_or(1))
            .collect::<HashSet<_>>();
        let season = (seasons.len() == 1).then(|| *seasons.iter().next().unwrap_or(&1));
        let job_id = uuid::Uuid::new_v4().to_string();
        let ledger_ids = units
            .iter()
            .map(|unit| unit.row.id.to_string())
            .collect::<Vec<_>>();
        let specs = units
            .iter()
            .zip(&ledger_ids)
            .map(|(unit, ledger_id)| crate::store::ProbeJobUnitSpec {
                ledger_id,
                kind: unit.kind.as_str(),
                force_fingerprint: is_tv,
                reuse_fingerprint_cache: false,
                overwrite_markers: is_tv,
                reuse_media_info_cache: false,
            })
            .collect::<Vec<_>>();
        let created = self.store.lock().create_probe_job(
            &job_id,
            job_kind,
            &media_id.to_string(),
            season,
            &scope,
            &specs,
        );
        match created {
            Ok(true) => {}
            Ok(false) => {
                let conflict = ledger_ids.iter().any(|ledger_id| {
                    self.store
                        .lock()
                        .active_probe_unit_for_ledger(ledger_id)
                        .ok()
                        .flatten()
                        .is_some()
                });
                return Err(if conflict {
                    ProbeEnqueueError::AlreadyRunning
                } else {
                    ProbeEnqueueError::Persistence
                });
            }
            Err(error) => {
                tracing::error!(%error, %media_id, "创建手动媒体探测任务失败");
                return Err(ProbeEnqueueError::Persistence);
            }
        }

        let refresh_id = is_tv.then(|| self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed));
        let mut queued = 0;
        for unit in &mut units {
            unit.force_fingerprint = is_tv;
            unit.reuse_fingerprint_cache = false;
            unit.reuse_media_info_cache = false;
            unit.overwrite_markers = is_tv;
            unit.marker_refresh_id = refresh_id;
            unit.job_id = Some(job_id.clone());
            self.seen.lock().insert(unit.row.id.to_string());
            if self.queue_metadata(unit.clone()).is_ok() {
                queued += 1;
            } else {
                self.finish(unit, false);
            }
        }
        tracing::info!(
            %media_id,
            job_id,
            seasons = seasons.len(),
            queued,
            total = units.len(),
            "【媒体探测】手动整条目刷新已持久化入队，旧结果保持可见"
        );
        Ok(queued)
    }

    /// 是否已在排队/执行（前端「探测中」状态用）。
    pub fn is_queued(&self, ledger_id: &str) -> bool {
        self.store
            .lock()
            .is_probe_queued(ledger_id)
            .unwrap_or_else(|_| self.seen.lock().contains(ledger_id))
    }

    pub(crate) fn marker_refresh_job(
        &self,
        media_id: MediaId,
        season: u32,
    ) -> Result<Option<crate::store::ProbeJob>, crate::store::StoreError> {
        self.store
            .lock()
            .latest_marker_refresh_for_season(&media_id.to_string(), season)
    }

    pub(super) fn active_probe_job_id(&self, ledger_id: &str) -> Option<String> {
        self.store
            .lock()
            .active_probe_unit_for_ledger(ledger_id)
            .ok()
            .flatten()
            .map(|unit| unit.job_id)
    }

    #[cfg(test)]
    pub(crate) fn take_queued_for_test(&self) -> Option<ProbeUnit> {
        self.metadata_rx.try_lock().ok()?.try_recv().ok()
    }

    #[cfg(test)]
    pub(crate) fn has_fingerprint_queued_for_test(&self) -> bool {
        let Ok(mut rx) = self.fingerprint_rx.try_lock() else {
            return false;
        };
        match rx.try_recv() {
            Ok(work) => {
                drop(rx);
                self.fingerprint_tx.send(work).is_ok()
            }
            Err(_) => false,
        }
    }

    fn finish(&self, unit: &ProbeUnit, succeeded: bool) {
        let status = if succeeded { "succeeded" } else { "failed" };
        self.finish_with_status(unit, status, None, (!succeeded).then_some("探测单元执行失败"));
    }

    pub(crate) fn finish_with_status(
        &self,
        unit: &ProbeUnit,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) {
        let ledger_id = unit.row.id.to_string();
        let cancelled = unit
            .job_id
            .as_deref()
            .and_then(|id| self.store.lock().get_probe_job(id).ok().flatten())
            .is_some_and(|job| job.status == "cancelled");
        if cancelled {
            self.seen.lock().remove(&ledger_id);
            return;
        }
        self.finish_persisted_unit_with_status(unit, status, error_kind, error);
        self.seen.lock().remove(&ledger_id);
        if status == "failed" {
            self.mark_failed(&ledger_id);
        }
    }

    pub(crate) fn cancel_unit(&self, unit: &ProbeUnit, reason: &str) {
        let ledger_id = unit.row.id.to_string();
        self.seen.lock().remove(&ledger_id);
        let Some(job_id) = unit.job_id.as_deref() else {
            return;
        };
        let result = {
            let store = self.store.lock();
            store
                .cancel_probe_unit(job_id, &ledger_id, reason)
                .and_then(|(job, _, all_done)| {
                    // A refresh is published atomically; membership changes cancel the entire batch.
                    if job.kind == "marker_refresh" || all_done {
                        store.cancel_probe_job(job_id, reason)?;
                    }
                    store.get_probe_job(job_id)
                })
        };
        match result {
            Ok(Some(job)) => {
                if !job.is_active() {
                    if let Ok(units) = self.store.lock().probe_job_units(job_id) {
                        let mut seen = self.seen.lock();
                        for pending in units {
                            seen.remove(&pending.ledger_id);
                        }
                    }
                    timings::log_terminal_job(&self.store, &self.timings, &job);
                }
                tracing::info!(job_id, ledger_id, reason, status = %job.status,
                    elapsed_ms = job.elapsed_ms(now_ms()), "【媒体探测】任务已取消");
            }
            Ok(None) => tracing::error!(job_id, ledger_id, "取消时探测任务不存在"),
            Err(error) => tracing::error!(%error, job_id, ledger_id, "持久化探测任务取消状态失败"),
        }
    }

    fn finish_persisted_unit(&self, unit: &ProbeUnit, succeeded: bool) {
        let status = if succeeded { "succeeded" } else { "failed" };
        self.finish_persisted_unit_with_status(unit, status, None, (!succeeded).then_some("探测单元执行失败"));
    }

    pub(crate) fn finish_persisted_unit_with_status(
        &self,
        unit: &ProbeUnit,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) {
        let ledger_id = unit.row.id.to_string();
        let Some(job_id) = unit.job_id.as_deref() else {
            tracing::error!(ledger_id, "探测队列项缺少持久化任务 ID，拒绝写入结果");
            return;
        };
        let result = self.store.lock().finish_probe_unit_with_status(
            job_id,
            &ledger_id,
            status,
            error_kind,
            error,
        );
        let (job, accepted, all_done) = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, ledger_id, job_id, "持久化探测任务结果失败");
                return;
            }
        };
        if !accepted {
            tracing::warn!(job_id, ledger_id, "忽略已结束或重复提交的探测单元结果");
            return;
        }
        tracing::info!(
            job_id = %job.id,
            media_id = %job.media_id,
            season = job.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            completed = job.completed,
            total = job.total,
            status,
            "【媒体探测】任务单元结果已写入数据库"
        );
        if !all_done {
            return;
        }
        if job.failed > 0 {
            if let Err(error) = self.store.lock().finish_probe_job(
                &job.id,
                false,
                job.error.as_deref().or(Some("一个或多个探测单元失败")),
            ) {
                tracing::error!(%error, job_id = %job.id, "持久化探测任务失败状态失败");
            }
            tracing::warn!(
                job_id = %job.id,
                media_id = %job.media_id,
                completed = job.completed,
                total = job.total,
                elapsed_ms = job.elapsed_ms(now_ms()),
                "【探测任务】部分单元失败，保留现有片头片尾结果"
            );
            timings::log_terminal_job(&self.store, &self.timings, &job);
        } else if job.kind == "marker_refresh" {
            marker_jobs::apply_marker_refresh(self, &job, unit);
            timings::log_terminal_job(&self.store, &self.timings, &job);
        } else {
            let final_succeeded = job.status == "succeeded";
            let job_final_status = job.status.clone();
            match self.store.lock().finish_probe_job(&job.id, final_succeeded, job.error.as_deref()) {
                Ok(()) => tracing::info!(
                    job_id = %job.id,
                    media_id = %job.media_id,
                    completed = job.completed,
                    total = job.total,
                    status = %job_final_status,
                    elapsed_ms = job.elapsed_ms(now_ms()),
                    "【媒体探测】持久化任务完成"
                ),
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, "持久化探测任务成功状态失败")
                }
            }
            timings::log_terminal_job(&self.store, &self.timings, &job);
        }
    }

    fn persist_single_unit(&self, unit: &ProbeUnit, kind: &str) -> Option<String> {
        let job_id = uuid::Uuid::new_v4().to_string();
        let ledger_id = unit.row.id.to_string();
        let scope = format!("ledger:{ledger_id}");
        let spec = crate::store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
            kind: unit.kind.as_str(),
            force_fingerprint: unit.force_fingerprint,
            reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
            overwrite_markers: unit.overwrite_markers,
            reuse_media_info_cache: unit.reuse_media_info_cache,
        };
        match self.store.lock().create_probe_job(
            &job_id,
            kind,
            &unit.row.media_id.to_string(),
            unit.row.season,
            &scope,
            &[spec],
        ) {
            Ok(true) => Some(job_id),
            Ok(false) => None,
            Err(error) => {
                tracing::error!(%error, ledger_id, "无法持久化探测任务");
                None
            }
        }
    }

    fn mark_failed(&self, ledger_id: &str) {
        let mut failures = self.last_failure.lock();
        failures.retain(|_, at| at.elapsed() < Duration::from_secs(60));
        failures.insert(ledger_id.to_string(), Instant::now());
    }

    pub fn try_start_workers(self: &Arc<Self>) -> bool {
        worker::try_start(self)
    }
}

pub(crate) fn marker_refresh_scope(media_id: MediaId, season: u32) -> String {
    format!("marker-refresh:{media_id}:{season}")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::markers::persist_marker;
    use super::probe::fingerprint_duration;
    use super::*;
    use domain::{Confidence, LedgerId, MediaId, QualitySource};
    use std::path::PathBuf;

    mod queue {
        use super::*;
        include!("probe_manager/queue_tests.rs");
    }

    mod marker_refresh {
        use super::*;
        include!("probe_manager/marker_refresh_tests.rs");
    }

    mod marker_persist {
        use super::*;
        include!("probe_manager/marker_persist_tests.rs");
    }
}
