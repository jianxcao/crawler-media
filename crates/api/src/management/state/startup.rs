//! Instance startup after owned services have been constructed.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use downloader::Downloader;
use indexer::{Fetcher, Indexer, RegistrySnapshot};
use parking_lot::Mutex;

use super::{ApiState, ApiStateError, STORE_READERS, apply_admin_bootstrap, open_jobs, seed_jobs};
use crate::Store;
use crate::catalog::{Catalog, EmptyCatalog};
use crate::poster_fetch::HttpPoster;
use crate::store::StoreError;
use crate::system_logs::{DEFAULT_CAPACITY, LogBuffer};

pub struct ApiStateBootstrapOptions {
    pub log_buffer: Option<LogBuffer>,
    pub browser: Option<Arc<indexer::Browser>>,
    pub self_listen: Option<SocketAddr>,
    pub indexer_overlay_dir: Option<PathBuf>,
    pub initial_writable: bool,
    pub pause_background_probe: bool,
}

impl Default for ApiStateBootstrapOptions {
    fn default() -> Self {
        Self {
            log_buffer: None,
            browser: None,
            self_listen: None,
            indexer_overlay_dir: None,
            initial_writable: true,
            pause_background_probe: false,
        }
    }
}

type ProbeCatalogInit = (
    Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    Arc<crate::probe_manager::ProbeManager>,
    Arc<crate::chapter_frame_service::ChapterFrameService>,
    Arc<dyn Catalog>,
    Arc<dyn crate::media_posters::MediaPosterSource>,
);

fn init_probe_and_catalog(
    store: &Arc<Store>,
    initial_writable: bool,
    pause_background_probe: bool,
) -> Result<ProbeCatalogInit, ApiStateError> {
    let library_work = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
    let probe = Arc::new(match (initial_writable, pause_background_probe) {
        (true, true) => crate::probe_manager::ProbeManager::new_paused(store.clone())?,
        (true, false) => crate::probe_manager::ProbeManager::new(store.clone())?,
        (false, true) => crate::probe_manager::ProbeManager::new_deferred_paused(store.clone())?,
        (false, false) => crate::probe_manager::ProbeManager::new_deferred(store.clone())?,
    });
    probe.set_library_registry(library_work.clone());
    let chapter_frames = Arc::new(crate::chapter_frame_service::ChapterFrameService::new(
        store.clone(),
        library_work.clone(),
    ));
    probe.set_chapter_frames(chapter_frames.clone());
    let catalog: Arc<dyn Catalog> = Arc::new(EmptyCatalog);
    let media_posters: Arc<dyn crate::media_posters::MediaPosterSource> = Arc::new(
        crate::media_posters::service::MediaPosterService::new(store.clone(), catalog.clone()),
    );
    Ok((library_work, probe, chapter_frames, catalog, media_posters))
}

pub(super) fn assemble_api_state<F>(
    store: Arc<Store>,
    snapshot: Arc<RegistrySnapshot>,
    fetcher: Arc<F>,
    downloader: Arc<dyn Downloader>,
    deferred_bootstrap: Option<crate::bootstrap_credentials::AdminBootstrapAction>,
    opts: ApiStateBootstrapOptions,
) -> Result<ApiState, ApiStateError>
where
    F: Fetcher + 'static,
{
    let initial_writable = opts.initial_writable;
    let jellyfin_server_id = jellyfin_server_id(store.as_ref(), initial_writable)?;
    let jobs_path = store.as_ref().data_dir().join("jobs.db");
    let jobs = Arc::new(crate::JobsWriter::new(open_jobs(
        &jobs_path,
        initial_writable,
    )?));
    let jobs_readers = crate::jobs_readers::open(&jobs_path)?;
    store.configure_reader_pool_size(
        store::Db::Library,
        NonZeroUsize::new(STORE_READERS)
            .ok_or_else(|| StoreError::Invalid("Library reader capacity is zero".into()))?,
    )?;
    let library_reads = store.reader_pool(store::Db::Library);
    let data_dir = store.data_dir().to_path_buf();
    let writers = store.writers();
    // 有 Tokio runtime 时启动异步探测 worker；只读从属实例延迟恢复与后台启动。
    let (library_work, probe, chapter_frames, catalog, media_posters) =
        init_probe_and_catalog(&store, initial_writable, opts.pause_background_probe)?;
    let (browser, browser_err) = match opts.browser {
        Some(browser) => (browser, None),
        None => {
            let (b, err) = crate::runtime_browser::startup_browser(store.clone());
            (Arc::new(b), err)
        }
    };
    let plugins = Arc::new(crate::plugins::PluginService::with_store_and_jobs(
        store.clone(),
        jobs.clone(),
    ));
    let state = build_api_state_struct(
        store,
        library_reads,
        writers,
        library_work,
        jobs,
        jobs_readers,
        initial_writable,
        deferred_bootstrap,
        jellyfin_server_id,
        snapshot,
        fetcher,
        downloader,
        catalog,
        media_posters,
        data_dir,
        browser,
        probe,
        plugins,
        chapter_frames,
        opts.log_buffer
            .unwrap_or_else(|| LogBuffer::new(DEFAULT_CAPACITY)),
        opts.self_listen,
        opts.indexer_overlay_dir,
    );
    state.sync_plugin_host_deps();
    state.finish_initialization(browser_err)
}

#[allow(clippy::too_many_arguments)]
fn build_api_state_struct<F>(
    store: Arc<Store>,
    library_reads: Arc<store::ReaderPool>,
    writers: Arc<store::Writers>,
    library_work: Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    jobs: Arc<crate::JobsWriter>,
    jobs_readers: Arc<store::ReaderPool>,
    initial_writable: bool,
    deferred_bootstrap: Option<crate::bootstrap_credentials::AdminBootstrapAction>,
    jellyfin_server_id: String,
    snapshot: Arc<RegistrySnapshot>,
    fetcher: Arc<F>,
    downloader: Arc<dyn Downloader>,
    catalog: Arc<dyn Catalog>,
    media_posters: Arc<dyn crate::media_posters::MediaPosterSource>,
    data_dir: PathBuf,
    browser: Arc<indexer::Browser>,
    probe: Arc<crate::probe_manager::ProbeManager>,
    plugins: Arc<crate::plugins::PluginService>,
    chapter_frames: Arc<crate::chapter_frame_service::ChapterFrameService>,
    log_buffer: LogBuffer,
    self_listen: Option<SocketAddr>,
    indexer_overlay_dir: Option<PathBuf>,
) -> ApiState
where
    F: Fetcher + 'static,
{
    ApiState {
        store,
        library_reads,
        writers,
        password_login: Arc::new(crate::password_login::PasswordLogin::new(
            Arc::new(crate::password_login::Argon2Engine),
            2,
        )),
        http_work: Arc::new(crate::blocking_budget::BlockingBudget::new(8)),
        chapter_work: Arc::new(crate::blocking_budget::BlockingBudget::new(2)),
        discover_work: Arc::new(Default::default()),
        search_work: Arc::new(crate::blocking_budget::BlockingBudget::new(4)),
        season_enrichment_work: Arc::new(crate::blocking_budget::BlockingBudget::new(2)),
        fs_work: Arc::new(crate::blocking_budget::BlockingBudget::new(2)),
        artwork_reads: Arc::new(crate::blocking_budget::BlockingBudget::new(4)),
        jobs_read_work: Arc::new(crate::blocking_budget::BlockingBudget::new(4)),
        browser_reconcile: Default::default(),
        backdrop_enrichment: Arc::new(crate::backdrop_enrichment::BackdropEnrichment::new(2)),
        notify_transport: Arc::new(crate::http::notify::notify_transport::HttpNotifyTransport),
        subtitle_delivery: Arc::new(crate::subtitle_delivery::SubtitleDelivery::new(
            Arc::new(library::ControlledSubtitleRunner),
            2,
        )),
        file_stats: Arc::new(crate::file_stat_read::LocalFileStatRead),
        library_files: Arc::new(crate::library_presence::SystemLibraryFileRead),
        theintrodb_negative: Arc::new(Default::default()),
        theintrodb_factory: Arc::new(crate::theintrodb_lookup::HttpClientFactory),
        theintrodb_inflight: Arc::new(Default::default()),
        chapter_probe: Arc::new(crate::marker_resolver::FfprobeChapters),
        playback_clock: Arc::new(crate::playback_events::SystemPlaybackClock),
        job_updates: Arc::new(crate::job_updates::JobUpdates::new()),
        cover_service: Arc::new(crate::library_cover_service::LibraryCoverService::local()),
        scan_coordinator: Arc::new(crate::scan_operations::ScanCoordinator::new()),
        library_work,
        jobs,
        jobs_readers,
        job_loop_control: Arc::new(crate::job_loop::JobLoopControl::new()),
        instance_writable: Arc::new(std::sync::atomic::AtomicBool::new(initial_writable)),
        writable_startup_done: Arc::new(std::sync::atomic::AtomicBool::new(initial_writable)),
        runtime_workers_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        deferred_bootstrap: Arc::new(Mutex::new(deferred_bootstrap)),
        write_gate: Arc::new(crate::write_gate::WriteGate::new()),
        maintenance_executor: Arc::new(crate::maintenance::SqliteExecutor),
        maintenance_lock: Arc::new(Mutex::new(())),
        media_refs_retry_at: Arc::new(Mutex::new((0, None))),
        subscribe_cleanup_cursor: Arc::new(Mutex::new(None)),
        subscribe_jobs_cursor: Arc::new(Mutex::new(None)),
        active_job_attempts: Arc::new(Mutex::new(HashMap::new())),
        job_attempt_coordinator: Arc::new(Mutex::new(())),
        subscribe_guards: Arc::new(Mutex::new(HashMap::new())),
        active_transfers: Arc::new(Mutex::new(HashSet::new())),
        subscribe_round_finished: Arc::new(tokio::sync::Notify::new()),
        deleting_subscribes: Arc::new(Mutex::new(HashSet::new())),
        manual_admit_inflight: Arc::new(Mutex::new(HashSet::new())),
        in_flight_download_ops: Arc::new(Mutex::new(HashMap::new())),
        jellyfin_server_id: Arc::new(jellyfin_server_id),
        indexer: Arc::new(Indexer::new(snapshot, fetcher)),
        downloader,
        torrent_payload_fetcher: Arc::new(
            crate::download_service::fetcher::NetGuardTorrentFetcher::default(),
        ),
        catalog: catalog.clone(),
        configuration_memory: Arc::new(crate::configuration_memory::ConfigurationMemory::default()),
        similar_reads: Arc::default(),
        scan_catalog: Arc::new(Mutex::new((catalog, media_posters.clone()))),
        poster_fetch: Arc::new(HttpPoster),
        fanart_cache: Arc::default(),
        media_posters,
        image_delivery: crate::http::image_proxy::new(&data_dir),
        log_buffer,
        obscura: Arc::new(crate::obscura_manager::ObscuraManager::new(&data_dir)),
        browser,
        probe,
        plugins,
        self_listen,
        indexer_overlay_dir,
        plugin_http_policy_override: None,
        admin_trusted_plugins: HashSet::new(),
        chapter_frames,
        jellyfin_candidates: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        jellyfin_heavy_items: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        jellyfin_hydrated_rows: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        query_phase_totals: Arc::new(super::QueryPhaseTotals::default()),
        protocol_selection_barrier: None,
        library_header_barrier: None,
        library_read_stage_barrier: None,
        statistics_phase_barrier: None,
        fingerprint_settings_after_write_barrier: None,
    }
}

impl ApiState {
    /// Production server constructor: background bulk media probing starts paused
    /// so leftover queue backlogs never hammer remote mounts until resumed in UI.
    pub fn new_production_with_admin_bootstrap_writable<F>(
        store: Arc<Store>,
        snapshot: Arc<RegistrySnapshot>,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        action: crate::bootstrap_credentials::AdminBootstrapAction,
        initial_writable: bool,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        Self::new_arc_with_bootstrap_options(
            store,
            snapshot,
            fetcher,
            downloader,
            action,
            ApiStateBootstrapOptions {
                initial_writable,
                pause_background_probe: true,
                ..Default::default()
            },
        )
    }

    pub(super) fn finish_initialization(
        self,
        browser_error: Option<String>,
    ) -> Result<Self, ApiStateError> {
        if let Some(error) = browser_error {
            self.obscura.set_last_error(Some(error));
        }
        if self.instance_writable() {
            crate::plugins::builtins::bootstrap_builtin_plugins(&self);
            crate::plugins::sync_registry_snapshot_on_startup(&self);
            self.ensure_writable_runtime_started()?;
            // Replay an interrupted cover removal after database startup.
            crate::library_cover_service::purge_removed_library_covers(&self);
        }
        Ok(self)
    }

    pub fn ensure_writable_runtime_started(&self) -> Result<(), ApiStateError> {
        if !self.writable_startup_done.swap(true, Ordering::AcqRel)
            && let Err(error) = self.seed_writable_startup_state()
        {
            self.writable_startup_done.store(false, Ordering::Release);
            return Err(error);
        }
        self.start_runtime()?;
        Ok(())
    }

    fn seed_writable_startup_state(&self) -> Result<(), ApiStateError> {
        if let Some(action) = self.deferred_bootstrap.lock().take() {
            apply_admin_bootstrap(&self.store.as_ref(), action)?;
        }
        if self.store.get_setting("jellyfin.server_id")?.is_none() {
            self.store
                .put_setting("jellyfin.server_id", &self.jellyfin_server_id)?;
        }
        {
            let jobs = self.jobs.acquire()?;
            seed_jobs(&jobs)?;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            self.probe.recover_deferred();
        }
        crate::plugins::builtins::bootstrap_builtin_plugins(self);
        crate::plugins::sync_registry_snapshot_on_startup(self);
        crate::library_cover_service::purge_removed_library_covers(self);
        Ok(())
    }

    fn start_runtime(&self) -> Result<(), ApiStateError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Ok(());
        }
        if self.runtime_workers_started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        crate::http::browser_settings::reconcile_startup(self);
        if let Err(error) = crate::library_lifecycle::resume_library_removals(self) {
            self.runtime_workers_started.store(false, Ordering::Release);
            return Err(std::io::Error::other(error).into());
        }
        crate::plugins::events::recover_startup(
            self,
            crate::plugins::events::unix_now_ms_fallback(),
        );
        self.probe.try_start_workers();
        self.chapter_frames.start();
        crate::scan_probe_dispatch::start_dispatcher(self.clone());
        // T11: Old browse projection periodic refresh is obsolete with library_items single table
        match self.store.as_ref().recover_orphaned_scans() {
            Ok(n) if n > 0 => tracing::info!(count = n, "已恢复未完成的扫描操作"),
            Ok(_) => {}
            Err(e) => tracing::error!(%e, "恢复未完成的扫描操作失败"),
        }
        crate::root_reassign_work::start(self);
        crate::rename_work::resume_startup(self.clone());
        Ok(())
    }
}

/// Persist the protocol identity once when writable; standbys keep an in-memory fallback.
pub(super) fn jellyfin_server_id(
    store: &crate::Store,
    writable: bool,
) -> Result<String, crate::store::StoreError> {
    if let Some(id) = store.get_setting("jellyfin.server_id")? {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    if writable {
        store.put_setting("jellyfin.server_id", &id)?;
    }
    Ok(id)
}
