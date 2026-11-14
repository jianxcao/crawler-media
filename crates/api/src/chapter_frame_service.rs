//! Durable chapter scene-frame dispatch.
//!
//! The Store owns the obligation; this service owns only a bounded window of
//! claimed IDs. At most [`MAX_CHAPTER_PROCESSES`] frames render at once and at
//! most [`DISPATCH_WINDOW`] obligations are held in memory, so a season sweep
//! can enqueue hundreds of episodes without spawning hundreds of processes.

mod execution;
mod identity;
pub use identity::chapter_frame_artifact_path;
mod recovery;
pub mod worker;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::Store;
use crate::http::store_wait::store_blocking;
use crate::library_lifecycle::{LibraryWorkLease, LibraryWorkRegistry};
use crate::store::{
    ChapterFrameArtifact, ChapterFrameClaim, ChapterFrameState, ChapterFrameTargetResult,
    ChapterFrameTask,
};
use library::WorkControl;
pub use store::chapter_frame_generation;

/// Rendering processes allowed at once. ffmpeg is the only expensive resource
/// this service owns, so this is the process bound.
pub const MAX_CHAPTER_PROCESSES: usize = 2;
/// Obligations claimed from the durable queue and held in memory.
pub const DISPATCH_WINDOW: usize = 64;

const POLL: Duration = Duration::from_millis(25);
// Integrity release has no chapter-service notification. Recheck only while
// durable work remains deferred; a fully empty queue stays notification-only.
const DEFERRED_RECHECK: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct ChapterFrameRequest {
    pub file_id: domain::LedgerId,
    pub video: PathBuf,
    pub source_version: String,
    pub targets: Vec<(i64, i64)>,
    pub duration_ms: Option<i64>,
    pub generation: String,
    pub attempt: u64,
    pub pending_indices: Vec<usize>,
    pub candidates: Vec<(usize, ChapterFrameArtifact)>,
}

/// One rendered frame waiting for the publication gate.
#[derive(Clone, Debug)]
pub struct ChapterFrameStaged {
    pub staged: PathBuf,
    pub target: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ChapterFrameRenderResult {
    pub index: usize,
    pub staged: Result<ChapterFrameStaged, String>,
}

/// Frame renderer seam. Production uses ffmpeg through [`worker`]; tests inject
/// a gated fake so no real process starts.
pub trait ChapterFrameRunner: Send + Sync {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String>;
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChapterFrameMeasurement {
    pub file_id: domain::LedgerId,
    pub task_id: String,
    pub generation: String,
    pub attempt: u64,
    pub frame_ms: Option<u64>,
    pub publish_ms: Option<u64>,
    pub task_ms: u64,
    pub adopted: Option<bool>,
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<ChapterFrameClaim>,
    running: BTreeMap<(String, u64), domain::LedgerId>,
    dispatch_after: Option<String>,
}

pub struct ChapterFrameService {
    measurements: Mutex<VecDeque<ChapterFrameMeasurement>>,
    measurements_dropped: AtomicUsize,
    store: Arc<Store>,
    registry: Arc<LibraryWorkRegistry>,
    runner: Arc<dyn ChapterFrameRunner>,
    inner: Mutex<Inner>,
    fill_lock: Mutex<()>,
    notify: Notify,
    started: AtomicBool,
    shutting_down: AtomicBool,
    deferred_refill: AtomicBool,
    workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    published_images: AtomicUsize,
}

impl ChapterFrameService {
    pub fn new(store: Arc<Store>, registry: Arc<LibraryWorkRegistry>) -> Self {
        Self::with_runner(
            store,
            registry,
            Arc::new(worker::FfmpegChapterFrameRunner::default()),
        )
    }

    pub fn with_runner(
        store: Arc<Store>,
        registry: Arc<LibraryWorkRegistry>,
        runner: Arc<dyn ChapterFrameRunner>,
    ) -> Self {
        Self {
            measurements: Mutex::new(VecDeque::new()),
            measurements_dropped: AtomicUsize::new(0),
            store,
            registry,
            runner,
            inner: Mutex::new(Inner::default()),
            fill_lock: Mutex::new(()),
            notify: Notify::new(),
            started: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            deferred_refill: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
            published_images: AtomicUsize::new(0),
        }
    }

    pub fn measurement_snapshot(&self) -> (Vec<ChapterFrameMeasurement>, usize) {
        (
            self.measurements.lock().iter().cloned().collect(),
            self.measurements_dropped.load(Ordering::Relaxed),
        )
    }

    fn record_measurement(&self, measurement: ChapterFrameMeasurement) {
        let mut measurements = self.measurements.lock();
        if measurements.len() >= 2048 {
            measurements.pop_front();
            self.measurements_dropped.fetch_add(1, Ordering::Relaxed);
        }
        measurements.push_back(measurement);
    }

    /// Start the fixed worker set. Without a Tokio runtime nothing is spawned:
    /// enqueueing stays durable-only, which is the no-runtime contract.
    pub fn start(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if self.started.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
            return;
        }
        self.shutting_down.store(false, Ordering::SeqCst);
        self.deferred_refill.store(false, Ordering::SeqCst);
        self.inner.lock().dispatch_after = None;
        let mut workers = self.workers.lock();
        let service = self.clone();
        workers.push(tokio::spawn(async move {
            let recovery = service.clone();
            if let Err(error) = Self::blocking_step(recovery, |service| service.recover_sync())
                .await
                .and_then(|result| result)
            {
                tracing::warn!(%error, "章节帧任务启动恢复失败");
                service.started.store(false, Ordering::SeqCst);
                return;
            }
            tokio::join!(service.clone().worker_loop(), service.worker_loop());
        }));
        drop(workers);
        self.notify.notify_waiters();
    }

    /// Persist one obligation for a file and wake the workers.
    pub fn enqueue_ledger(&self, file_id: &str, targets: Vec<(i64, i64)>) -> Result<bool, String> {
        let file_id = file_id_from_str(file_id)?;
        let (path, enabled) = {
            let store = self.store.as_ref();
            let Some(row) = store
                .file_coverage_by_id(file_id)
                .map_err(|error| error.to_string())?
            else {
                return Ok(false);
            };
            let path = row.file.path;
            let enabled = crate::http::library_chapters::chapter_images_enabled(&store, &path);
            (path, enabled)
        };
        if !enabled {
            return Ok(false);
        }
        self.enqueue_path_inner(file_id, &path, targets, true)
    }

    /// Persist one obligation resolved from a file path.
    pub fn enqueue_path(
        &self,
        path: &Path,
        targets: Vec<(i64, i64)>,
        retry_failed: bool,
    ) -> Result<bool, String> {
        let file_id = {
            let store = self.store.as_ref();
            store
                .file_id_at_path(&path.to_string_lossy())
                .map_err(|error| error.to_string())?
        };
        match file_id {
            Some(file_id) => {
                self.enqueue_path_inner(file_id_from_str(&file_id)?, path, targets, retry_failed)
            }
            None => Ok(false),
        }
    }

    fn enqueue_path_inner(
        &self,
        file_id: domain::LedgerId,
        path: &Path,
        targets: Vec<(i64, i64)>,
        retry_failed: bool,
    ) -> Result<bool, String> {
        let Some(file) = self
            .store
            .file_coverage_by_id(file_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        if file.file.path != path {
            return Err("chapter frame path changed".into());
        }
        let source_version = file.file.version.source_version.clone();
        if crate::fingerprint_job::current_source_version(path) != source_version {
            return Err("chapter frame source changed before enqueue".into());
        }
        let previous = self
            .store
            .current_chapter_frame_task(file_id)
            .map_err(|error| error.to_string())?;
        if let Some(snapshot) = &previous {
            self.repair_missing_sync(snapshot, path)?;
        }
        let task = ChapterFrameTask {
            id: uuid::Uuid::new_v4().to_string(),
            file_id,
            source_version,
            targets_hash: chapter_frame_generation(
                file_id,
                &file.file.version.source_version,
                &targets,
                1,
            ),
            targets,
        };
        let status = self
            .store
            .as_ref()
            .ensure_chapter_frame_task(&task, retry_failed)
            .map_err(|error| error.to_string())?;
        let inserted = status.total > 0
            && (status.task_id == task.id
                || previous.is_some_and(|previous| {
                    previous.status.state != ChapterFrameState::Queued
                        && status.state == ChapterFrameState::Queued
                }));
        if status.state == ChapterFrameState::Queued {
            self.notify.notify_waiters();
        }
        Ok(inserted)
    }

    /// Publication has already committed its durable obligation on the Library
    /// connection. A lost wakeup is recoverable from that obligation at startup.
    pub fn notify_pending(&self) {
        self.notify.notify_waiters();
    }

    /// Claim queued obligations up to the in-memory window. Returns how many
    /// were newly claimed.
    pub async fn dispatch_once(self: &Arc<Self>) -> Result<usize, String> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err("chapter frame service is shut down".into());
        }
        let claimed = self.fill_window().await?;
        if claimed > 0 || self.deferred_refill.load(Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
        Ok(claimed)
    }

    /// Obligations currently held in memory (queued + rendering).
    pub fn pending_memory(&self) -> usize {
        let inner = self.inner.lock();
        inner.queue.len() + inner.running.len()
    }

    /// Whether this file still owns a queued claim or an executing frame worker.
    pub(crate) fn has_pending_file(&self, file_id: domain::LedgerId) -> bool {
        let inner = self.inner.lock();
        inner
            .queue
            .iter()
            .any(|claim| claim.task.file_id == file_id)
            || inner.running.values().any(|running| *running == file_id)
    }

    /// Successfully published frames. Test seam and capacity observability.
    pub fn published_images(&self) -> usize {
        self.published_images.load(Ordering::SeqCst)
    }

    /// Best-effort synchronous teardown for service replacement. Unlike
    /// [`Self::shutdown`], a blocking render may still be finishing; its claim
    /// stays `running` and is recovered on the next start.
    pub fn abort(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        for handle in self.workers.lock().drain(..) {
            handle.abort();
        }
        {
            let mut inner = self.inner.lock();
            inner.queue.clear();
            inner.dispatch_after = None;
        }
        self.deferred_refill.store(false, Ordering::SeqCst);
        self.started.store(false, Ordering::SeqCst);
    }

    /// Cancel in-flight work, join both workers, and return every unacknowledged
    /// claim to the durable queue.
    pub async fn shutdown(&self) -> Result<(), String> {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        let handles: Vec<_> = self.workers.lock().drain(..).collect();
        for handle in handles {
            let _ = handle.await;
        }
        {
            let mut inner = self.inner.lock();
            inner.queue.clear();
            inner.running.clear();
            inner.dispatch_after = None;
        }
        self.deferred_refill.store(false, Ordering::SeqCst);
        self.started.store(false, Ordering::SeqCst);
        crate::http::store_wait::store_blocking(self.store.clone(), |store| {
            store.recover_chapter_frame_tasks()
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Run one driver step on the blocking pool.
    ///
    /// Review L2 / Task 22: a driver is a task on the request runtime, so a
    /// `Store` lock taken in its async body parks a worker and stalls every other
    /// task — including `/health`, which touches no store at all. Waiting on a
    /// blocking thread costs one thread, never the executor.
    async fn blocking_step<T, F>(service: Arc<Self>, work: F) -> Result<T, String>
    where
        F: FnOnce(&Self) -> T + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(move || work(&service))
            .await
            .map_err(|error| format!("chapter frame worker failed: {error}"))
    }

    async fn fill_window(self: &Arc<Self>) -> Result<usize, String> {
        let service = self.clone();
        Self::blocking_step(service, |service| service.fill_window_sync()).await?
    }

    /// Claim pending obligations into the in-memory window.
    ///
    /// Sync by design: the caller runs it on the blocking pool (see [`Self::fill_window`]).
    fn fill_window_sync(&self) -> Result<usize, String> {
        let _fill = self.fill_lock.lock();
        let (capacity, after) = {
            let inner = self.inner.lock();
            (
                DISPATCH_WINDOW.saturating_sub(inner.queue.len() + inner.running.len()),
                inner.dispatch_after.clone(),
            )
        };
        if capacity == 0 {
            return Ok(0);
        }
        let pending = self
            .store
            .as_ref()
            .pending_chapter_frame_tasks_after(after.as_deref(), capacity)
            .map_err(|error| error.to_string())?;
        self.deferred_refill
            .store(!pending.is_empty(), Ordering::SeqCst);
        let full_page = pending.len() == capacity;
        let mut cursor = after;
        let mut claimed = 0;
        for task in pending {
            cursor = Some(task.id.clone());
            let owned = self
                .store
                .as_ref()
                .claim_chapter_frame_task(&task.id)
                .map_err(|error| error.to_string())?;
            if let Some(claim) = owned {
                self.inner.lock().queue.push_back(claim);
                claimed += 1;
            }
        }
        self.inner.lock().dispatch_after = if full_page { cursor } else { None };
        // A bounded head probe distinguishes deferred rows from a fully claimed
        // queue, so an empty service never keeps a periodic polling loop alive.
        let pending_remains = !self
            .store
            .pending_chapter_frame_tasks(1)
            .map_err(|error| error.to_string())?
            .is_empty();
        self.deferred_refill
            .store(pending_remains, Ordering::SeqCst);
        Ok(claimed)
    }

    fn take_next(&self) -> Option<ChapterFrameClaim> {
        let mut inner = self.inner.lock();
        let task = inner.queue.pop_front()?;
        inner
            .running
            .insert((task.task.id.clone(), task.attempt), task.task.file_id);
        Some(task)
    }

    fn finish_memory(&self, claim: &ChapterFrameClaim) {
        self.inner
            .lock()
            .running
            .remove(&(claim.task.id.clone(), claim.attempt));
    }

    async fn worker_loop(self: Arc<Self>) {
        loop {
            if self.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            // Register before filling so an enqueue between the empty check and
            // the await cannot lose its wakeup.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Err(error) = self.fill_window().await {
                tracing::warn!(%error, "章节帧任务派发失败");
            }
            match self.take_next() {
                Some(task) => self.run_task(task).await,
                None => {
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep(DEFERRED_RECHECK),
                            if self.deferred_refill.load(Ordering::SeqCst) => {}
                    }
                }
            }
        }
    }
}

/// A chapter obligation is keyed by the file id (`library_files.file_id`).
fn file_id_from_str(value: &str) -> Result<domain::LedgerId, String> {
    domain::LedgerId::from_str(&value.replace('-', ""))
        .map_err(|error| format!("invalid file id: {error}"))
}
