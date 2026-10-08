//! 异步媒体探测队列（ProbeManager）。
//!
//! 详情页 / Jellyfin 请求**绝不等待探测**：未缓存的文件立即入队，由后台
//! worker 串行探测（默认并发 1，兼容 115 等一次只放一个请求的网盘），结果
//! 写 file_meta / NFO / 片头片尾标记，前端轮询即可看到。一次探测任务同时
//! 产出 streamdetails（视频/音轨/字幕 → file_meta + NFO）与单集声纹指纹
//! （→ 指纹缓存，同季 ≥2 集后自动比对识别片头）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use domain::{LedgerRow, MediaId, MediaKind};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use marker::{ChromaprintEngine, FingerprintEngine};

use crate::Store;

mod marker_jobs;
mod markers;
mod probe;
mod timings;

/// 一个待探测单元（对应一条 ledger 行）。
#[derive(Clone)]
pub struct ProbeUnit {
    pub row: LedgerRow,
    pub kind: MediaKind,
    pub force_fingerprint: bool,
    pub reuse_fingerprint_cache: bool,
    pub overwrite_markers: bool,
    pub(crate) marker_refresh_id: Option<u64>,
    /// Persisted task identity carried with the queue message. This prevents
    /// stale messages from attaching to a later retry for the same ledger row.
    pub(crate) job_id: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ProbeEnqueueError {
    AlreadyRunning,
    Persistence,
}

pub struct ProbeManager {
    store: Arc<Mutex<Store>>,
    fingerprint_engine: Arc<dyn FingerprintEngine>,
    tx: mpsc::UnboundedSender<ProbeUnit>,
    /// 幂等去重：正在排队或执行中的 ledger_id。
    seen: Mutex<HashSet<String>>,
    last_failure: Mutex<HashMap<String, Instant>>,
    next_marker_refresh_id: AtomicU64,
    timings: timings::ProbeTimingLedger,
    /// 多 worker 共享的任务流（tokio Mutex 的 guard 可安全跨 await）。
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ProbeUnit>>>,
}

impl ProbeManager {
    pub fn new(store: Arc<Mutex<Store>>) -> Self {
        Self::with_fingerprint_engine(store, Arc::new(ChromaprintEngine))
    }

    pub fn with_fingerprint_engine(
        store: Arc<Mutex<Store>>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let manager = Self {
            store,
            fingerprint_engine,
            tx,
            seen: Mutex::new(HashSet::new()),
            last_failure: Mutex::new(HashMap::new()),
            next_marker_refresh_id: AtomicU64::new(1),
            timings: timings::ProbeTimingLedger::default(),
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
        };
        manager.recover_pending_jobs();
        manager
    }

    /// 入队探测（幂等：已在队列/执行中的跳过）。
    pub fn enqueue(&self, mut unit: ProbeUnit) -> bool {
        unit.reuse_fingerprint_cache = true;
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
            "【媒体探测】已入队后台探测（streamdetails + 声纹指纹）"
        );
        if self.tx.send(unit.clone()).is_err() {
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
        if self.tx.send(unit.clone()).is_err() {
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
                reuse_fingerprint_cache: true,
                overwrite_markers: true,
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
            unit.reuse_fingerprint_cache = true;
            unit.overwrite_markers = true;
            unit.marker_refresh_id = Some(id);
            unit.job_id = Some(job_id.clone());
            self.seen.lock().insert(unit.row.id.to_string());
            if self.tx.send(unit.clone()).is_ok() {
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
            unit.overwrite_markers = is_tv;
            unit.marker_refresh_id = refresh_id;
            unit.job_id = Some(job_id.clone());
            self.seen.lock().insert(unit.row.id.to_string());
            if self.tx.send(unit.clone()).is_ok() {
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
        self.rx.try_lock().ok()?.try_recv().ok()
    }

    fn finish(&self, unit: &ProbeUnit, succeeded: bool) {
        let ledger_id = unit.row.id.to_string();
        self.finish_persisted_unit(unit, succeeded);
        self.seen.lock().remove(&ledger_id);
        if !succeeded {
            self.mark_failed(&ledger_id);
        }
    }

    fn finish_persisted_unit(&self, unit: &ProbeUnit, succeeded: bool) {
        let ledger_id = unit.row.id.to_string();
        let Some(job_id) = unit.job_id.as_deref() else {
            tracing::error!(ledger_id, "探测队列项缺少持久化任务 ID，拒绝写入结果");
            return;
        };
        let result = self.store.lock().finish_probe_unit(
            job_id,
            &ledger_id,
            succeeded,
            (!succeeded).then_some("探测单元执行失败"),
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
            succeeded,
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
            match self.store.lock().finish_probe_job(&job.id, true, None) {
                Ok(()) => tracing::info!(
                    job_id = %job.id,
                    media_id = %job.media_id,
                    completed = job.completed,
                    total = job.total,
                    elapsed_ms = job.elapsed_ms(now_ms()),
                    "【媒体探测】持久化任务成功完成"
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

    fn recover_pending_jobs(&self) {
        let jobs = match self.store.lock().recover_probe_jobs() {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(%error, "读取待恢复媒体探测任务失败");
                return;
            }
        };
        for (job, units) in jobs {
            tracing::info!(
                job_id = %job.id,
                media_id = %job.media_id,
                status = %job.status,
                pending = units.iter().filter(|unit| unit.status == "queued").count(),
                completed = job.completed,
                total = job.total,
                "【探测任务】从数据库恢复未完成任务"
            );
            for persisted in units.iter().filter(|unit| unit.status == "queued") {
                let row = self
                    .store
                    .lock()
                    .get_ledger(&persisted.ledger_id.replace('-', ""));
                let row = match row {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        let _ = self.store.lock().finish_probe_unit(
                            &job.id,
                            &persisted.ledger_id,
                            false,
                            Some("媒体台账不存在，无法恢复探测"),
                        );
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(%error, job_id = %job.id, ledger_id = %persisted.ledger_id, "恢复探测任务读取台账失败");
                        if let Err(finish_error) = self.store.lock().finish_probe_unit(
                            &job.id,
                            &persisted.ledger_id,
                            false,
                            Some("恢复任务时读取媒体台账失败"),
                        ) {
                            tracing::error!(%finish_error, job_id = %job.id, ledger_id = %persisted.ledger_id, "持久化恢复失败的任务单元失败");
                        }
                        continue;
                    }
                };
                let kind = match persisted.kind.parse::<MediaKind>() {
                    Ok(kind) => kind,
                    Err(kind) => {
                        tracing::error!(job_id = %job.id, ledger_id = %persisted.ledger_id, %kind, "恢复探测任务媒体类型无效");
                        let _ = self.store.lock().finish_probe_unit(
                            &job.id,
                            &persisted.ledger_id,
                            false,
                            Some("恢复任务时媒体类型无效"),
                        );
                        continue;
                    }
                };
                self.seen.lock().insert(persisted.ledger_id.clone());
                let unit = ProbeUnit {
                    row,
                    kind,
                    force_fingerprint: persisted.force_fingerprint,
                    reuse_fingerprint_cache: persisted.reuse_fingerprint_cache,
                    overwrite_markers: persisted.overwrite_markers,
                    marker_refresh_id: (job.kind == "marker_refresh")
                        .then(|| self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed)),
                    job_id: Some(job.id.clone()),
                };
                if self.tx.send(unit).is_err() {
                    self.seen.lock().remove(&persisted.ledger_id);
                    tracing::error!(job_id = %job.id, ledger_id = %persisted.ledger_id, "恢复探测任务加入队列失败");
                    let _ = self.store.lock().finish_probe_unit(
                        &job.id,
                        &persisted.ledger_id,
                        false,
                        Some("恢复任务加入内存队列失败"),
                    );
                }
            }
            let refreshed = self.store.lock().get_probe_job(&job.id);
            let refreshed = match refreshed {
                Ok(Some(refreshed)) => refreshed,
                Ok(None) => {
                    tracing::error!(job_id = %job.id, "恢复探测任务后数据库中找不到任务记录");
                    continue;
                }
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, "恢复探测任务后读取任务状态失败");
                    match self
                        .store
                        .lock()
                        .fail_completed_probe_job(&job.id, "重启恢复时无法读取已完成任务状态")
                    {
                        Ok(true) => {
                            tracing::error!(job_id = %job.id, "已将无法确认状态的完成任务标记失败并释放任务范围")
                        }
                        Ok(false) => {
                            tracing::warn!(job_id = %job.id, "未终止探测任务：任务可能仍有待处理单元或已被其他流程终止")
                        }
                        Err(finish_error) => {
                            tracing::error!(%finish_error, job_id = %job.id, "读取任务状态失败后持久化失败状态也失败")
                        }
                    }
                    continue;
                }
            };
            if refreshed.completed >= refreshed.total && refreshed.is_active() {
                if refreshed.failed > 0 {
                    if let Err(error) = self.store.lock().finish_probe_job(
                        &job.id,
                        false,
                        refreshed.error.as_deref(),
                    ) {
                        tracing::error!(%error, job_id = %job.id, "持久化恢复任务的失败状态失败");
                    }
                } else if refreshed.kind == "marker_refresh" {
                    match self.recovered_marker_refresh_unit(&refreshed) {
                        Ok(unit) => {
                            marker_jobs::apply_marker_refresh(self, &refreshed, &unit);
                            timings::log_terminal_job(&self.store, &self.timings, &refreshed);
                        }
                        Err(error) => {
                            if let Err(finish_error) = self.store.lock().finish_probe_job(
                                &job.id,
                                false,
                                Some("重启恢复时无法读取任务台账"),
                            ) {
                                tracing::error!(%finish_error, job_id = %job.id, "持久化无法恢复的片头片尾任务失败状态失败");
                            }
                            tracing::error!(%error, job_id = %job.id, "完成的片头片尾任务无法恢复，已标记失败并释放任务范围");
                        }
                    }
                } else if let Err(error) = self.store.lock().finish_probe_job(&job.id, true, None) {
                    tracing::error!(%error, job_id = %job.id, "持久化恢复任务的成功状态失败");
                }
            }
        }
    }

    fn recovered_marker_refresh_unit(
        &self,
        job: &crate::store::ProbeJob,
    ) -> Result<ProbeUnit, crate::store::StoreError> {
        let store = self.store.lock();
        let persisted = store
            .probe_job_units(&job.id)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!("units for job {}", job.id))
            })?;
        let row = store
            .get_ledger(&persisted.ledger_id.replace('-', ""))?
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!("ledger {}", persisted.ledger_id))
            })?;
        Ok(ProbeUnit {
            row,
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: persisted.reuse_fingerprint_cache,
            overwrite_markers: true,
            marker_refresh_id: Some(self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed)),
            job_id: Some(job.id.clone()),
        })
    }

    fn mark_failed(&self, ledger_id: &str) {
        let mut failures = self.last_failure.lock();
        failures.retain(|_, at| at.elapsed() < Duration::from_secs(60));
        failures.insert(ledger_id.to_string(), Instant::now());
    }

    /// 启动 N 个 worker（N = `probe.concurrency`，默认 1，上限 8）。
    /// 只在 Tokio runtime 上下文生效（生产入口在 Tokio main 内调用）；
    /// 测试/非 runtime 路径仅创建队列，worker 不启动、探测不入队执行。
    pub fn try_start_workers(self: &Arc<Self>) -> bool {
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!(
                "【媒体探测】当前不在 Tokio runtime 上下文，探测 worker 未启动（测试/非生产路径）"
            );
            return false;
        }
        let concurrency: usize = self
            .store
            .lock()
            .get_setting(crate::settings_keys::PROBE_CONCURRENCY)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(1)
            .min(8);
        for _ in 0..concurrency {
            let mgr = self.clone();
            tokio::spawn(async move {
                loop {
                    let unit = {
                        let mut rx = mgr.rx.lock().await;
                        rx.recv().await
                    };
                    let Some(unit) = unit else {
                        break;
                    };
                    let ledger_id = unit.row.id.to_string();
                    let start = unit
                        .job_id
                        .as_deref()
                        .map(|job_id| mgr.store.lock().start_probe_unit(job_id, &ledger_id));
                    let started = match start {
                        Some(Ok(started)) => started,
                        Some(Err(error)) => {
                            tracing::error!(%error, ledger_id, job_id = ?unit.job_id, "探测任务开始状态写入失败");
                            mgr.finish(&unit, false);
                            false
                        }
                        None => {
                            tracing::error!(ledger_id, "队列单元缺少任务 ID，无法启动持久化任务");
                            false
                        }
                    };
                    if !started {
                        tracing::warn!(
                            ledger_id,
                            "队列单元已被其他 worker 处理或数据库状态缺失，跳过重复执行"
                        );
                        mgr.seen.lock().remove(&ledger_id);
                        continue;
                    }
                    let succeeded = probe::probe_one(&mgr, &unit).await;
                    mgr.finish(&unit, succeeded);
                }
            });
        }
        tracing::info!(
            concurrency,
            "【媒体探测】异步探测队列已启动（worker={concurrency}）"
        );
        true
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
