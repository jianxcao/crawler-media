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
    covers_season, ledger_id, require_episode_coordinates, require_single_unit, single_episode,
    single_season, single_season_opt,
};
#[cfg(test)]
pub(crate) use projection::{single_episode_opt, synthetic_coverage};

mod admission;
mod analysis;
pub mod clock;
mod dispatch;
mod lifecycle;
mod markers;
mod media_info;
pub(crate) use media_info::MediaInfoRefreshError;
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
pub mod season_schedule;
pub mod stages;
mod store_access;
mod store_lease;
pub(crate) mod timings;
mod tracks;
mod uncertainty;
mod worker;
pub use store_access::ProbeStoreAccess;
pub use tracks::TracksProbe;

#[cfg(test)]
mod library_removal_worker_tests;
#[cfg(test)]
mod locking_tests;
#[cfg(test)]
mod publish_version_tests;
#[cfg(test)]
mod settings_publish_tests;
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
    audio_selections: Arc<Mutex<HashMap<std::path::PathBuf, u32>>>,
    store: Arc<Store>,
    store_key: std::path::PathBuf,
    store_lease: Mutex<Option<store_lease::Lease>>,
    recovery_deferred: AtomicBool,
    library_work: Mutex<Option<Arc<crate::library_lifecycle::LibraryWorkRegistry>>>,
    chapter_frames: Mutex<Option<Arc<crate::chapter_frame_service::ChapterFrameService>>>,
    fingerprint_engine: Arc<marker::matcher::session::SeasonMatchSession>,
    capture_engine: Arc<dyn FingerprintCaptureEngine>,
    tracks_probe: Arc<dyn TracksProbe>,
    metadata_tx: mpsc::Sender<store::ProbeDispatchId>,
    fingerprint_tx: mpsc::Sender<(store::ProbeDispatchId, probe::FingerprintWork)>,
    metadata_overflowed: AtomicBool,
    fingerprint_overflowed: AtomicBool,
    /// 幂等去重：窗口内或执行中的 `ledger_id -> job_id`，不是全量积压。
    seen: Mutex<HashMap<String, String>>,
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
    clock: Mutex<Arc<dyn clock::ProbeClock>>,
    pub comparison_runs: std::sync::atomic::AtomicUsize,
    analysis_schedule: Mutex<season_schedule::SeasonAnalysisSchedule>,
    /// Times `request_probe` assessed a row. Selection must not assess rows
    /// that are not on the selected page.
    pub assessments: AtomicUsize,
    workers_started: AtomicBool,
    background_paused: AtomicBool,
    paused_exempt_jobs: std::sync::Mutex<HashSet<String>>,
    paused_exempt_units: Mutex<HashSet<(String, String)>>,
    obligation_wake: Arc<Notify>,
    probe_settings: Mutex<crate::probe_settings::ProbeSettings>,
    remote_reads_paused: Mutex<bool>,
    settings_publish_lock: Mutex<()>,
    settings_revision: AtomicU64,
    remote_reads_wait_stopped: Arc<AtomicBool>,
    metadata_admission: Mutex<worker::MetadataAdmission>,
    metadata_capacity: Notify,
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

    pub fn new_paused(store: Arc<Store>) -> Result<Self, store::StoreError> {
        Self::with_engines_paused(
            store,
            Arc::new(ChromaprintEngine),
            Arc::new(ChromaprintEngine),
            true,
        )
    }

    pub(crate) fn new_deferred(store: Arc<Store>) -> Result<Self, store::StoreError> {
        Self::new_deferred_with_pause(store, false)
    }

    pub(crate) fn new_deferred_paused(store: Arc<Store>) -> Result<Self, store::StoreError> {
        Self::new_deferred_with_pause(store, true)
    }

    fn new_deferred_with_pause(
        store: Arc<Store>,
        initial_background_paused: bool,
    ) -> Result<Self, store::StoreError> {
        Self::with_comparison_session_inner(
            store,
            Arc::new(marker::matcher::session::SeasonMatchSession::new(
                Arc::new(ChromaprintEngine),
                marker::matcher::session::DEFAULT_MAX_PAIRS,
                marker::matcher::session::DEFAULT_MAX_BYTES,
            )),
            Arc::new(ChromaprintEngine),
            false,
            initial_background_paused,
        )
    }

    pub fn with_engines(
        store: Arc<Store>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
    ) -> Result<Self, store::StoreError> {
        Self::with_engines_paused(store, fingerprint_engine, capture_engine, false)
    }

    pub fn with_engines_paused(
        store: Arc<Store>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
        initial_background_paused: bool,
    ) -> Result<Self, store::StoreError> {
        Self::with_comparison_session_inner(
            store,
            Arc::new(marker::matcher::session::SeasonMatchSession::new(
                fingerprint_engine,
                marker::matcher::session::DEFAULT_MAX_PAIRS,
                marker::matcher::session::DEFAULT_MAX_BYTES,
            )),
            capture_engine,
            true,
            initial_background_paused,
        )
    }

    #[doc(hidden)]
    pub fn with_engines_unrecovered(
        store: Arc<Store>,
        fingerprint_engine: Arc<dyn FingerprintEngine>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
    ) -> Result<Self, store::StoreError> {
        Self::with_comparison_session_inner(
            store,
            Arc::new(marker::matcher::session::SeasonMatchSession::new(
                fingerprint_engine,
                marker::matcher::session::DEFAULT_MAX_PAIRS,
                marker::matcher::session::DEFAULT_MAX_BYTES,
            )),
            capture_engine,
            false,
            false,
        )
    }

    /// Inject one immutable comparison owner while keeping natural durable job,
    /// capture and publication behavior. Zero bounds select uncached comparison.
    pub fn with_comparison_session(
        store: Arc<Store>,
        fingerprint_engine: Arc<marker::matcher::session::SeasonMatchSession>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
    ) -> Result<Self, store::StoreError> {
        Self::with_comparison_session_inner(store, fingerprint_engine, capture_engine, true, false)
    }

    fn with_comparison_session_inner(
        store: Arc<Store>,
        fingerprint_engine: Arc<marker::matcher::session::SeasonMatchSession>,
        capture_engine: Arc<dyn FingerprintCaptureEngine>,
        recover_now: bool,
        initial_background_paused: bool,
    ) -> Result<Self, store::StoreError> {
        let channels = dispatch::channels();
        let settings = crate::probe_settings::ProbeSettings::load(&store)?;
        let remote_reads = crate::probe_settings::RemoteReadsSettings::load(&store)?;
        let (shutdown_signal, _) = tokio::sync::watch::channel(false);
        let data_dir = store.data_dir().to_path_buf();
        let store_key = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);
        let manager = Self {
            audio_selections: Arc::new(Mutex::new(HashMap::new())),
            store,
            store_key,
            store_lease: Mutex::new(None),
            recovery_deferred: AtomicBool::new(!recover_now),
            library_work: Mutex::new(None),
            chapter_frames: Mutex::new(None),
            fingerprint_engine,
            capture_engine,
            tracks_probe: Arc::new(tracks::FfprobeTracks),
            metadata_tx: channels.metadata_tx,
            fingerprint_tx: channels.fingerprint_tx,
            metadata_overflowed: AtomicBool::new(false),
            fingerprint_overflowed: AtomicBool::new(false),
            seen: Mutex::new(HashMap::new()),
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
            clock: Mutex::new(Arc::new(clock::SystemClock)),
            comparison_runs: std::sync::atomic::AtomicUsize::new(0),
            analysis_schedule: Mutex::new(season_schedule::SeasonAnalysisSchedule::new(
                2_000, 10_000,
            )),
            assessments: AtomicUsize::new(0),
            workers_started: AtomicBool::new(false),
            background_paused: AtomicBool::new(initial_background_paused),
            paused_exempt_jobs: std::sync::Mutex::new(HashSet::new()),
            paused_exempt_units: Mutex::new(HashSet::new()),
            obligation_wake: Arc::new(Notify::new()),
            probe_settings: Mutex::new(settings),
            remote_reads_paused: Mutex::new(remote_reads.paused),
            settings_publish_lock: Mutex::new(()),
            settings_revision: AtomicU64::new(0),
            remote_reads_wait_stopped: Arc::new(AtomicBool::new(remote_reads.paused)),
            metadata_admission: Mutex::new(worker::MetadataAdmission::new(
                settings.metadata_workers,
            )),
            metadata_capacity: Notify::new(),
            shutting_down: Arc::new(AtomicBool::new(false)),
            shutdown_signal,
            shutdown_lock: tokio::sync::Mutex::new(()),
            shutdown_drain: Mutex::new(None),
            workers: Mutex::new(Vec::new()),
            wake: Notify::new(),
        };
        if recover_now && !store_lease::recover_if_idle(&manager) {
            manager.recovery_deferred.store(true, Ordering::SeqCst);
        }
        Ok(manager)
    }

    pub(crate) fn recover_deferred(&self) {
        if self.recovery_deferred.swap(false, Ordering::SeqCst)
            && !store_lease::recover_if_idle(self)
        {
            self.recovery_deferred.store(true, Ordering::SeqCst);
        }
    }

    /// Replace stream IO while retaining the same queue and publication pipeline.
    pub fn with_tracks_probe(mut self, probe: Arc<dyn TracksProbe>) -> Self {
        self.tracks_probe = probe;
        self
    }

    /// Explicit capture selection; the existing source-versioned inventory validates it.
    pub fn select_capture_audio(&self, path: std::path::PathBuf, global_index: u32) {
        self.audio_selections.lock().insert(path, global_index);
    }

    fn capture_source(&self, file: &domain::LibraryFile) -> Option<marker::CaptureSourceFacts> {
        let selected = self.audio_selections.lock().get(&file.path).copied();
        crate::fingerprint_job::source_for_file_selected(&self.store, file, selected)
    }

    pub fn timing_snapshot(&self, job_id: &str) -> Option<timings::ProbeTimingSummary> {
        self.timings.snapshot(job_id)
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

    #[doc(hidden)]
    pub fn try_start_workers_observed(
        self: &Arc<Self>,
        after_settings_loaded: impl FnOnce(),
    ) -> bool {
        worker::try_start_observed(self, after_settings_loaded)
    }

    pub(crate) fn apply_probe_settings(&self, settings: crate::probe_settings::ProbeSettings) {
        let _publish = self.settings_publish_lock.lock();
        self.apply_probe_settings_locked(settings);
        self.settings_revision.fetch_add(1, Ordering::Release);
    }

    fn apply_probe_settings_locked(&self, settings: crate::probe_settings::ProbeSettings) {
        let mut current = self.probe_settings.lock();
        *current = settings;
        self.configure_probe_admission(settings);
    }

    pub(crate) fn apply_remote_reads_pause(&self, paused: bool) {
        let _publish = self.settings_publish_lock.lock();
        self.apply_remote_reads_pause_locked(paused);
        self.settings_revision.fetch_add(1, Ordering::Release);
    }

    fn apply_remote_reads_pause_locked(&self, paused: bool) {
        let mut current = self.remote_reads_paused.lock();
        *current = paused;
        self.remote_reads_wait_stopped.store(
            paused || self.shutting_down.load(Ordering::Acquire),
            Ordering::Release,
        );
        marker::media_read::set_remote_reads_paused(paused);
        drop(current);
        self.wake.notify_waiters();
    }

    fn sync_remote_reads_wait_stopped_locked(&self) {
        let paused = self.remote_reads_paused.lock();
        self.remote_reads_wait_stopped.store(
            *paused || self.shutting_down.load(Ordering::Acquire),
            Ordering::Release,
        );
    }

    pub(crate) fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    pub(crate) fn apply_startup_settings_if_unchanged(
        &self,
        observed_revision: u64,
        settings: Option<crate::probe_settings::ProbeSettings>,
        remote_reads: Option<crate::probe_settings::RemoteReadsSettings>,
    ) -> bool {
        let _publish = self.settings_publish_lock.lock();
        if self.settings_revision.load(Ordering::Acquire) != observed_revision {
            self.sync_remote_reads_wait_stopped_locked();
            return false;
        }
        if let Some(settings) = settings {
            self.apply_probe_settings_locked(settings);
        }
        self.apply_remote_reads_pause_locked(remote_reads.is_none_or(|settings| settings.paused));
        self.settings_revision.fetch_add(1, Ordering::Release);
        true
    }

    pub(crate) fn remote_reads_are_paused(&self) -> bool {
        *self.remote_reads_paused.lock()
    }

    fn configure_probe_admission(&self, settings: crate::probe_settings::ProbeSettings) {
        self.metadata_admission.lock().limit = settings.metadata_workers;
        marker::media_read::configure_remote_reads(
            settings.remote_concurrency,
            Duration::from_millis(settings.remote_cooldown_ms),
        );
        self.metadata_capacity.notify_waiters();
    }

    pub async fn shutdown_workers(&self) -> Result<(), String> {
        ProbeManager::shutdown_dispatch(self).await
    }
    pub fn set_clock(&self, clock: Arc<dyn clock::ProbeClock>) {
        *self.clock.lock() = clock;
    }
    pub fn wake_dispatch(&self) {
        self.wake.notify_waiters();
    }

    pub fn background_is_paused(&self) -> bool {
        self.background_paused.load(Ordering::Acquire)
    }

    pub fn apply_startup_pause_policy(&self) {
        self.pause_background();
    }

    pub fn pause_background(&self) {
        self.background_paused.store(true, Ordering::Release);
        self.clear_paused_exempt_jobs();
        self.analysis_schedule.lock().clear_unstarted();
        self.drain_buffered_dispatch();
    }

    pub(crate) fn set_background_paused_flag(&self, paused: bool) {
        self.background_paused.store(paused, Ordering::Release);
    }

    pub fn resume_background(&self) {
        self.background_paused.store(false, Ordering::Release);
        self.clear_paused_exempt_jobs();
        self.fill_dispatch_windows();
        self.wake.notify_waiters();
        self.obligation_wake.notify_waiters();
    }

    pub(super) fn has_seen(&self, ledger_id: &str) -> bool {
        self.seen.lock().contains_key(ledger_id)
    }

    pub(super) fn try_insert_seen(&self, ledger_id: String, job_id: String) -> bool {
        let mut seen = self.seen.lock();
        if seen.contains_key(&ledger_id) {
            return false;
        }
        seen.insert(ledger_id, job_id);
        true
    }

    pub(super) fn set_seen(&self, ledger_id: String, job_id: String) {
        self.seen.lock().insert(ledger_id, job_id);
    }

    pub(super) fn remove_seen_if_job(&self, ledger_id: &str, job_id: &str) -> bool {
        let mut seen = self.seen.lock();
        remove_seen_entry_if_job(&mut seen, ledger_id, job_id)
    }

    pub(super) fn remove_seen_any(&self, ledger_id: &str) -> bool {
        self.seen.lock().remove(ledger_id).is_some()
    }

    pub(super) fn mark_job_paused_exempt(&self, job_id: String) -> bool {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id)
    }

    pub(super) fn unmark_job_paused_exempt(&self, job_id: &str) {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job_id);
        self.paused_exempt_units.lock().retain(|(j, _)| j != job_id);
    }

    pub(super) fn is_job_paused_exempt(&self, job_id: &str) -> bool {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(job_id)
    }

    pub(super) fn mark_unit_paused_exempt(&self, job_id: String, ledger_id: String) -> bool {
        self.paused_exempt_units.lock().insert((job_id, ledger_id))
    }

    pub(super) fn unmark_unit_paused_exempt(&self, job_id: &str, ledger_id: &str) {
        self.paused_exempt_units
            .lock()
            .retain(|(j, l)| !(j == job_id && l == ledger_id));
    }

    pub(super) fn is_unit_paused_exempt(&self, job_id: &str, ledger_id: &str) -> bool {
        self.paused_exempt_units
            .lock()
            .iter()
            .any(|(j, l)| j == job_id && l == ledger_id)
    }

    pub(super) fn is_dispatch_paused_exempt(&self, job_id: &str, ledger_id: &str) -> bool {
        self.is_job_paused_exempt(job_id) || self.is_unit_paused_exempt(job_id, ledger_id)
    }

    pub(super) fn has_paused_exempt_jobs(&self) -> bool {
        !self
            .paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub fn has_paused_exempt_work(&self) -> bool {
        self.has_paused_exempt_jobs() || !self.paused_exempt_units.lock().is_empty()
    }

    pub(super) fn paused_exempt_job_ids(&self) -> Vec<String> {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub(super) fn paused_exempt_unit_pairs(&self) -> Vec<(String, String)> {
        self.paused_exempt_units.lock().iter().cloned().collect()
    }

    pub(super) fn paused_exempt_jobs_for_ledgers(&self, ledger_ids: &[String]) -> HashSet<String> {
        let mut jobs = self
            .paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let units = self.paused_exempt_units.lock();
        for (job_id, lid) in units.iter() {
            if ledger_ids.iter().any(|target| target == lid) {
                jobs.insert(job_id.clone());
            }
        }
        jobs
    }

    pub(super) fn prune_inactive_exempt_jobs(&self, checked: &[String], active: &[String]) {
        let active_set: HashSet<&str> = active.iter().map(String::as_str).collect();
        let mut guard = self
            .paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for job_id in checked {
            if !active_set.contains(job_id.as_str()) {
                guard.remove(job_id);
            }
        }
    }

    pub(super) fn prune_inactive_exempt_units(
        &self,
        checked: &[(String, String)],
        active: &[(String, String)],
    ) {
        let active_set: HashSet<&(String, String)> = active.iter().collect();
        let mut guard = self.paused_exempt_units.lock();
        for pair in checked {
            if !active_set.contains(pair) {
                guard.remove(pair);
            }
        }
    }

    pub(super) fn clear_paused_exempt_jobs(&self) {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.paused_exempt_units.lock().clear();
    }

    pub(crate) fn obligation_wake(&self) -> Arc<Notify> {
        Arc::clone(&self.obligation_wake)
    }

    pub(crate) fn purge_cancelled_seen(&self, file_ids: &[String], global_cancel: bool) {
        if global_cancel {
            self.drain_buffered_dispatch();
        }
        {
            let mut seen = self.seen.lock();
            for file_id in file_ids {
                seen.remove(file_id);
            }
        }
        if !self.background_is_paused() || self.has_paused_exempt_work() {
            self.wake.notify_waiters();
        }
    }
}

pub(super) fn remove_seen_entry_if_job(
    seen: &mut HashMap<String, String>,
    ledger_id: &str,
    job_id: &str,
) -> bool {
    if seen.get(ledger_id).is_some_and(|active| active == job_id) {
        seen.remove(ledger_id);
        true
    } else {
        false
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
    use super::probe::fingerprint_duration_for_store;
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
