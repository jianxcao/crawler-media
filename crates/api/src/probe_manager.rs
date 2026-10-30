//! 异步媒体探测队列（ProbeManager）。
//!
//! 详情页 / Jellyfin 请求**绝不等待探测**：未缓存的文件立即入队，由后台
//! worker 先提取并持久化媒体流信息，再将可选的片头片尾声纹投递到后台队列。
//! 详情页 / Jellyfin 请求不等待探测，结果写入数据库供前端轮询。

use domain::{FileCoverage, MediaId, MediaKind};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};

use marker::fingerprint::capture_types::FingerprintCaptureEngine;
use marker::{ChromaprintEngine, FingerprintEngine};

use crate::Store;

pub(crate) use projection::{
    coverage_for_ledger_id, covers_season, ledger_id, require_episode_coordinates,
    require_single_unit, single_episode, single_season, single_season_opt,
};
#[cfg(test)]
pub(crate) use projection::{single_episode_opt, synthetic_coverage};

mod admission;
pub mod clock;
mod dispatch;
mod lifecycle;
mod marker_jobs;
mod markers;
pub mod policy;
mod presence;
mod probe;
pub mod progress;
mod projection;
mod query;
mod queue;
pub mod recovery;
mod refresh;
mod retry;
pub mod season;
pub mod stages;
mod store_access;
mod store_lease;
pub(crate) mod timings;
mod tracks;
mod worker;
pub use store_access::ProbeStoreAccess;
pub use tracks::TracksProbe;

#[cfg(test)]
mod locking_tests;
#[cfg(test)]
mod publish_version_tests;
#[cfg(test)]
mod test_support;

/// 一个待探测单元（对应一个文件的覆盖事实 `FileCoverage`：文件 + 单元）。
#[derive(Clone)]
pub struct ProbeUnit {
    pub row: FileCoverage,
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
    Admission(axum::response::Response),
}

pub struct ProbeManager {
    store: Arc<Store>,
    store_key: std::path::PathBuf,
    store_lease: Mutex<Option<store_lease::Lease>>,
    recovery_deferred: AtomicBool,
    library_work: Mutex<Option<Arc<crate::library_lifecycle::LibraryWorkRegistry>>>,
    chapter_frames: Mutex<Option<Arc<crate::chapter_frame_service::ChapterFrameService>>>,
    fingerprint_engine: Arc<dyn FingerprintEngine>,
    capture_engine: Arc<dyn FingerprintCaptureEngine>,
    tracks_probe: Arc<dyn TracksProbe>,
    metadata_tx: mpsc::Sender<store::ProbeDispatchId>,
    fingerprint_tx: mpsc::Sender<(store::ProbeDispatchId, probe::FingerprintWork)>,
    /// 幂等去重：窗口内或执行中的 ledger_id，不是全量积压。
    seen: Mutex<HashSet<String>>,
    last_failure: Mutex<HashMap<String, Instant>>,
    next_marker_refresh_id: AtomicU64,
    pub(crate) metadata_pending: Arc<AtomicUsize>,
    pub(crate) metadata_idle: Arc<Notify>,
    pub(crate) metadata_idle_generation: AtomicUsize,
    pub(crate) timings: timings::ProbeTimingLedger,
    metadata_rx: Mutex<Option<mpsc::Receiver<store::ProbeDispatchId>>>,
    metadata_receiver: tokio::sync::Mutex<()>,
    dispatch_work: crate::blocking_budget::BlockingBudget,
    fingerprint_rx: Mutex<Option<mpsc::Receiver<(store::ProbeDispatchId, probe::FingerprintWork)>>>,
    pub(crate) recovered_marker_refreshes: Mutex<Vec<(crate::store::ProbeJob, ProbeUnit)>>,
    clock: Mutex<Arc<dyn clock::ProbeClock>>,
    pub comparison_runs: std::sync::atomic::AtomicUsize,
    /// Times `request_probe` assessed a row. Selection must not assess rows
    /// that are not on the selected page.
    pub assessments: AtomicUsize,
    workers_started: AtomicBool,
    shutting_down: Arc<AtomicBool>,
    shutdown_signal: tokio::sync::watch::Sender<bool>,
    shutdown_lock: tokio::sync::Mutex<()>,
    shutdown_drain: Mutex<Option<tokio::sync::watch::Receiver<bool>>>,
    workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    wake: Notify,
}

impl ProbeManager {
    pub fn new(store: Arc<Store>) -> Result<Self, store::StoreError> {
        Self::with_engines(
            store,
            Arc::new(ChromaprintEngine),
            Arc::new(ChromaprintEngine),
        )
    }

    pub fn with_fingerprint_engine(
        store: Arc<Store>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
    ) -> Result<Self, store::StoreError> {
        Self::with_engines(store, fingerprint_engine, Arc::new(ChromaprintEngine))
    }

    pub fn with_engines(
        store: Arc<Store>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
    ) -> Result<Self, store::StoreError> {
        let channels = dispatch::channels();
        let (shutdown_signal, _) = tokio::sync::watch::channel(false);
        let data_dir = store.data_dir().to_path_buf();
        let store_key = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);
        let manager = Self {
            store,
            store_key,
            store_lease: Mutex::new(None),
            recovery_deferred: AtomicBool::new(false),
            library_work: Mutex::new(None),
            chapter_frames: Mutex::new(None),
            fingerprint_engine,
            capture_engine,
            tracks_probe: Arc::new(tracks::FfprobeTracks),
            metadata_tx: channels.metadata_tx,
            fingerprint_tx: channels.fingerprint_tx,
            seen: Mutex::new(HashSet::new()),
            last_failure: Mutex::new(HashMap::new()),
            next_marker_refresh_id: AtomicU64::new(1),
            metadata_pending: Arc::new(AtomicUsize::new(0)),
            metadata_idle: Arc::new(Notify::new()),
            metadata_idle_generation: AtomicUsize::new(0),
            timings: timings::ProbeTimingLedger::default(),
            metadata_rx: Mutex::new(Some(channels.metadata_rx)),
            metadata_receiver: tokio::sync::Mutex::new(()),
            dispatch_work: crate::blocking_budget::BlockingBudget::new(12),
            fingerprint_rx: Mutex::new(Some(channels.fingerprint_rx)),
            recovered_marker_refreshes: Mutex::new(Vec::new()),
            clock: Mutex::new(Arc::new(clock::SystemClock)),
            comparison_runs: std::sync::atomic::AtomicUsize::new(0),
            assessments: AtomicUsize::new(0),
            workers_started: AtomicBool::new(false),
            shutting_down: Arc::new(AtomicBool::new(false)),
            shutdown_signal,
            shutdown_lock: tokio::sync::Mutex::new(()),
            shutdown_drain: Mutex::new(None),
            workers: Mutex::new(Vec::new()),
            wake: Notify::new(),
        };
        if !store_lease::recover_if_idle(&manager) {
            manager.recovery_deferred.store(true, Ordering::SeqCst);
        }
        Ok(manager)
    }

    /// Replace stream IO while retaining the same queue and publication pipeline.
    pub fn with_tracks_probe(mut self, probe: Arc<dyn TracksProbe>) -> Self {
        self.tracks_probe = probe;
        self
    }

    pub(crate) fn set_library_registry(
        &self,
        registry: Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    ) {
        *self.library_work.lock() = Some(registry);
    }

    pub(crate) fn set_chapter_frames(
        &self,
        service: Arc<crate::chapter_frame_service::ChapterFrameService>,
    ) {
        *self.chapter_frames.lock() = Some(service);
    }

    pub(crate) fn chapter_frames(
        &self,
    ) -> Option<Arc<crate::chapter_frame_service::ChapterFrameService>> {
        self.chapter_frames.lock().clone()
    }

    pub fn priority_gate(&self) -> Arc<dyn crate::fingerprint_job::CaptureGate> {
        Arc::new(progress::MetadataPriorityGate::from_manager(self))
    }

    /// When a recent failure still holds this file back, the time it may run again.
    fn failure_backoff_until_ms(&self, ledger_id: &str, now_ms: i64) -> Option<i64> {
        let failures = self.last_failure.lock();
        let elapsed = failures.get(ledger_id)?.elapsed();
        let window = Duration::from_secs(60);
        (elapsed < window).then(|| now_ms + (window - elapsed).as_millis() as i64)
    }

    fn mark_failed(&self, ledger_id: &str) {
        let mut failures = self.last_failure.lock();
        failures.retain(|_, at| at.elapsed() < Duration::from_secs(60));
        failures.insert(ledger_id.to_string(), Instant::now());
    }

    pub fn try_start_workers(self: &Arc<Self>) -> bool {
        worker::try_start(self)
    }

    pub async fn shutdown_workers(&self) -> Result<(), String> {
        ProbeManager::shutdown_dispatch(self).await
    }
    pub fn set_clock(&self, clock: Arc<dyn clock::ProbeClock>) {
        *self.clock.lock() = clock;
    }
}

pub(crate) fn marker_refresh_scope(media_id: MediaId, season: u32) -> String {
    format!("marker-refresh:{media_id}:{season}")
}

fn now_ms() -> i64 {
    clock::ProbeClock::now_ms(&clock::SystemClock)
}

#[cfg(test)]
mod tests {
    use super::markers::persist_marker;
    use super::probe::fingerprint_duration;
    use super::*;
    use domain::{LedgerId, MediaId};

    mod queue {
        use super::*;
        include!("probe_manager/queue_tests.rs");
    }

    mod marker_refresh {
        use super::*;
        include!("probe_manager/marker_refresh_tests.rs");
    }

    mod marker_refresh_lifecycle {
        use super::*;
        include!("probe_manager/marker_refresh_lifecycle_tests.rs");
        include!("probe_manager/marker_refresh_cross_library_tests.rs");
        include!("probe_manager/marker_refresh_lifecycle_worker_tests.rs");
        include!("probe_manager/optional_capture_cancellation_tests.rs");
    }

    mod marker_persist {
        use super::*;
        include!("probe_manager/marker_persist_tests.rs");
    }
}
