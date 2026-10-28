mod startup;

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use downloader::Downloader;
use indexer::{Fetcher, Indexer, ProfileSet};
use parking_lot::Mutex;

use super::query_phase::{QueryPhaseSnapshot, QueryPhaseTotals};
use crate::Store;
use crate::catalog::{Catalog, EmptyCatalog};
use crate::poster_fetch::{HttpPoster, PosterFetch};
use crate::store::StoreError;
use crate::system_logs::{DEFAULT_CAPACITY, LogBuffer};
use domain::{SubscribeId, UserId};

#[derive(Clone)]
pub struct ApiState {
    pub(crate) store: Arc<Mutex<Store>>,
    pub(crate) library_reads: Arc<store::LibraryReadPool>,
    library_read_shutdown: Arc<LibraryReadShutdown>,
    /// 身份读专用的 app.db 只读连接（Task 27）：token→用户 / 用户→角色 / 用户详情。
    ///
    /// 它**不参与** `store` 那把进程级锁，因此长写事务（扫描、完整性）期间成员请求
    /// 仍能完成鉴权；见 `store::AppReader`。
    pub(crate) app_reader: Arc<store::AppReader>,
    /// 两套登录协议共用的 offlock 密码校验与 CAS 签发服务。
    pub(crate) password_login: Arc<crate::password_login::PasswordLogin>,
    /// 有界 blocking 预算：owned 快照上的 stat/catalog 计算准入（生产默认 8）。
    pub http_work: Arc<crate::blocking_budget::BlockingBudget>,
    /// 搜索有界 blocking 预算（默认 4）。
    pub search_work: Arc<crate::blocking_budget::BlockingBudget>,
    /// 文件目录浏览有界 blocking 预算（默认 2）。
    pub fs_work: Arc<crate::blocking_budget::BlockingBudget>,
    /// Instance-owned coalesced Browser reconciliation; no global registry.
    pub(crate) browser_reconcile: Arc<crate::http::browser_settings::ReconcileCoordinator>,
    /// 后台背景图独立准入，按最终目标路径单飞；不占用交互式请求预算。
    pub(crate) backdrop_enrichment: Arc<crate::backdrop_enrichment::BackdropEnrichment>,
    /// notify 发送 seam（生产为显式超时的 HTTP 实现；测试可注入阻塞实现）。
    pub(crate) notify_transport: Arc<dyn crate::NotifyTransport>,
    /// 共享 single-flight 字幕交付（生产默认并发 2、受控 ffmpeg runner）。
    pub(crate) subtitle_delivery: Arc<crate::subtitle_delivery::SubtitleDelivery>,
    /// 浏览路径的只读文件访问（NFO / stat）；生产直接读磁盘，测试可注入阻塞实现。
    pub(crate) file_stats: Arc<dyn crate::file_stat_read::FileStatRead>,
    /// 文件系统观察 seam（verify / prune / watcher）；生产默认真实实现，测试可注入。
    pub(crate) library_files: Arc<dyn crate::library_presence::LibraryFileRead>,
    pub(crate) chapter_probe: Arc<dyn crate::marker_resolver::ChapterProbe>,
    /// 播放时钟 seam（生产为真实系统毫秒时钟；测试可注入假时钟）。
    pub(crate) playback_clock: Arc<dyn crate::playback_events::PlaybackClock>,
    /// 进程内共享的任务计数 watcher（所有 SSE 连接共用一个 poller）。
    /// 扫描操作协调器（持久单飞与并发控制）。
    pub scan_coordinator: Arc<crate::scan_operations::ScanCoordinator>,
    pub job_updates: Arc<crate::job_updates::JobUpdates>,
    /// Library 封面渲染与版本化发布（生产默认真实渲染器，测试可注入）。
    pub(crate) cover_service: Arc<crate::library_cover_service::LibraryCoverService>,
    pub(crate) library_work: Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    pub jobs: Arc<Mutex<jobs::Queue>>,
    /// 后台 job_loop 控制（停止意图与正在运行的 blocking worker 跟踪）。
    pub(crate) job_loop_control: Arc<crate::job_loop::JobLoopControl>,
    /// H-1: false while another instance holds the data-directory lease.
    /// Background writers stop and manual write endpoints answer 409.
    pub(crate) instance_writable: Arc<std::sync::atomic::AtomicBool>,
    /// L5: 维护期写门禁——关闸时前台写入口立刻 503 + `Retry-After`，
    /// 不在 SQLite busy 上耗 5 秒（与只读的 409 分开）。
    pub(crate) write_gate: Arc<crate::write_gate::WriteGate>,
    /// 数据库维护执行器（生产为真实 SQLite；测试可注入阻塞/报错的假实现）。
    pub(crate) maintenance_executor: Arc<dyn crate::maintenance::DatabaseExecutor>,
    /// 进程内维护互斥：`concurrency_key` 只防同一 def 重叠，这里覆盖不同维护 def。
    pub(crate) maintenance_lock: Arc<Mutex<()>>,
    /// Job attempts still executing in this process; protects them from timeout recovery.
    pub(crate) active_job_attempts: Arc<Mutex<HashMap<domain::JobId, jobs::Job>>>,
    /// Serializes a Subscribe pause/update against a worker's final add step.
    subscribe_guards: Arc<Mutex<HashMap<SubscribeId, Weak<Mutex<()>>>>>,
    /// Deletion reservations prevent workers and manual triggers from starting
    /// new work while asynchronous cleanup runs without holding a sync guard.
    deleting_subscribes: Arc<Mutex<HashSet<SubscribeId>>>,
    pub(crate) jellyfin_server_id: Arc<String>,
    pub(crate) indexer: Arc<Indexer>,
    pub(crate) downloader: Arc<dyn Downloader>,
    #[allow(dead_code)]
    pub(crate) library_root: Arc<PathBuf>,
    pub(crate) catalog: Arc<dyn Catalog>,
    pub(super) scan_catalog: Arc<
        Mutex<(
            Arc<dyn Catalog>,
            Arc<dyn crate::media_posters::MediaPosterSource>,
        )>,
    >,
    pub(crate) poster_fetch: Arc<dyn PosterFetch>,
    /// Fanart cache connection opened once per service lifetime (C03).
    pub(crate) fanart_cache: Arc<std::sync::OnceLock<Arc<media::cache::CatalogCache>>>,
    pub(crate) media_posters: Arc<dyn crate::media_posters::MediaPosterSource>,
    pub log_buffer: LogBuffer,
    pub obscura: Arc<crate::obscura_manager::ObscuraManager>,
    pub browser: Arc<indexer::Browser>,
    /// 异步媒体探测队列（streamdetails + 声纹指纹后台采集）。
    pub probe: Arc<crate::probe_manager::ProbeManager>,
    /// 持久章节帧义务的有界派发服务（2 进程 / 64 ID 窗口）。
    pub(crate) chapter_frames: Arc<crate::chapter_frame_service::ChapterFrameService>,
    /// Protocol references selected for Jellyfin pages.
    pub(crate) jellyfin_candidates: Arc<std::sync::atomic::AtomicUsize>,
    /// Jellyfin 选择里真正构建了重元数据（tracks/chapters/NFO/stat）的条目数：
    /// 证明只有选中的那一页被 enrich。
    pub(crate) jellyfin_heavy_items: Arc<std::sync::atomic::AtomicUsize>,
    /// Files, coverage units, Media and Playback rows captured for selected pages.
    pub(crate) jellyfin_hydrated_rows: Arc<std::sync::atomic::AtomicUsize>,
    query_phase_totals: Arc<QueryPhaseTotals>,
    /// Deterministic seam between protocol selection and selected fact capture.
    pub(crate) protocol_selection_barrier:
        Option<Arc<dyn Fn(&domain::ProtocolItemPage) + Send + Sync>>,
    pub(crate) library_header_barrier: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) library_read_stage_barrier: Option<Arc<dyn Fn(&'static str) + Send + Sync>>,
    /// 测试 seam：浏览请求真正解码的投影 file facts 行数（生产只读计数，无副作用）。
    /// 计数发生在**读取/解析处**，包含随后被丢弃或解析失败的行，以及依赖覆盖的
    /// 回退重读；热页只应解码选中 Media 的 facts。
    pub(crate) browse_page_decoded_files: Arc<std::sync::atomic::AtomicUsize>,
    /// 测试 seam：浏览请求真正解码的投影 media facts 行数（同上口径）。
    pub(crate) browse_page_decoded_media: Arc<std::sync::atomic::AtomicUsize>,
    /// 测试 seam：选页读过的轻候选 JSON 行数（常用排序的 SQL 分页应为 0）。
    pub(crate) browse_page_candidate_rows: Arc<std::sync::atomic::AtomicUsize>,
    /// 测试 seam：投影依赖覆盖读过的轻量行数（列读 + 候选行；重启后的首个小页
    /// 只应产生这一项成本，不应解码整份 facts）。
    pub(crate) browse_page_dependency_rows: Arc<std::sync::atomic::AtomicUsize>,
    /// 测试 seam：统计计算在计划收集与 totals 读取之间的屏障（生产为 `None`）。
    pub statistics_phase_barrier: Option<Arc<dyn Fn() + Send + Sync>>,
    /// 测试 seam：统计路径执行的用户 role 查询次数（生产只读计数）。
    #[allow(dead_code)]
    pub(crate) statistics_role_lookups: Arc<std::sync::atomic::AtomicUsize>,
}

struct LibraryReadShutdown(Arc<store::LibraryReadPool>);

impl Drop for LibraryReadShutdown {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn apply_admin_bootstrap(
    store: &Store,
    action: crate::bootstrap_credentials::AdminBootstrapAction,
) -> Result<(), StoreError> {
    let user_id = UserId::from_str(crate::bootstrap_credentials::STATIC_ADMIN_UUID_STR)
        .map_err(StoreError::from)?;
    match action {
        crate::bootstrap_credentials::AdminBootstrapAction::Seed {
            password,
            cli_token,
        } => {
            store.seed_admin_with_credentials(user_id, &password, &cli_token)?;
        }
        crate::bootstrap_credentials::AdminBootstrapAction::Keep { cli_token } => {
            let _ = store.force_admin_role(user_id);
            store.register_current_cli_token(user_id, &cli_token)?;
        }
    }
    Ok(())
}

impl ApiState {
    pub fn with_chapter_probe(
        mut self,
        probe: Arc<dyn crate::marker_resolver::ChapterProbe>,
    ) -> Self {
        self.chapter_probe = probe;
        self
    }

    /// Shared Fanart cache: schema validation/initialization runs on first use only.
    pub(crate) fn fanart_cache(&self) -> Result<Arc<media::cache::CatalogCache>, media::TmdbError> {
        if let Some(cache) = self.fanart_cache.get() {
            return Ok(cache.clone());
        }
        let path = self.store.lock().cache_db_path();
        let cache = media::fanart::open_cache(&path)?;
        Ok(self.fanart_cache.get_or_init(|| cache).clone())
    }

    pub fn new<F>(
        store: Store,
        token: String,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        let store = Arc::new(Mutex::new(store));
        Self::new_arc(store, token, profiles, fetcher, downloader, library_root)
    }

    /// Variant that takes the store already wrapped in Arc<Mutex<>>, so
    /// callers can share it with runtime-configurable clients (e.g. TMDB).
    pub fn new_arc<F>(
        store: Arc<Mutex<Store>>,
        token: String,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        Self::new_arc_with_admin_password(
            store,
            token,
            None,
            profiles,
            fetcher,
            downloader,
            library_root,
        )
    }

    pub fn new_arc_with_admin_password<F>(
        store: Arc<Mutex<Store>>,
        token: String,
        admin_password: Option<String>,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        let mode = if admin_password.is_some() {
            crate::bootstrap_credentials::BootstrapMode::Production
        } else {
            crate::bootstrap_credentials::BootstrapMode::TestFixture
        };
        let action = crate::bootstrap_credentials::prepare_admin_credentials(
            &store.lock(),
            &token,
            admin_password.as_deref(),
            mode,
        )
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

        Self::new_arc_with_admin_bootstrap(
            store,
            profiles,
            fetcher,
            downloader,
            library_root,
            action,
        )
    }

    pub fn new_arc_with_admin_bootstrap<F>(
        store: Arc<Mutex<Store>>,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
        action: crate::bootstrap_credentials::AdminBootstrapAction,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        std::fs::create_dir_all(&library_root)?;
        apply_admin_bootstrap(&store.lock(), action)?;
        let jellyfin_server_id = {
            let guard = store.lock();
            match guard.get_setting("jellyfin.server_id")? {
                Some(id) => id,
                None => {
                    let id = uuid::Uuid::new_v4().simple().to_string();
                    guard.put_setting("jellyfin.server_id", &id)?;
                    id
                }
            }
        };
        let jobs_path = store.lock().data_dir().join("jobs.db");
        let jobs = open_jobs(&jobs_path)?;
        let library_reads = Arc::new(
            store
                .lock()
                .open_library_read_pool(NonZeroUsize::new(2).unwrap())?,
        );
        let data_dir = store.lock().data_dir().to_path_buf();
        // Task 27：身份读走自己的 app.db 连接，不再排在 `store` 锁后面。
        let app_reader = Arc::new(store.lock().open_app_reader()?);
        // 异步探测队列：ApiState 创建时启动 worker（生产在 Tokio main 内，
        // 有 runtime 才真正 spawn；测试路径仅创建队列不启动）。
        let library_work = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
        let probe = Arc::new(crate::probe_manager::ProbeManager::new(store.clone()));
        probe.set_library_registry(library_work.clone());
        let startup_browser = crate::runtime_browser::startup_browser(store.clone());
        let chapter_frames = Arc::new(crate::chapter_frame_service::ChapterFrameService::new(
            store.clone(),
            library_work.clone(),
        ));
        probe.set_chapter_frames(chapter_frames.clone());
        let catalog: Arc<dyn Catalog> = Arc::new(EmptyCatalog);
        let media_posters: Arc<dyn crate::media_posters::MediaPosterSource> = Arc::new(
            crate::media_posters::service::MediaPosterService::new(store.clone(), catalog.clone()),
        );
        let state = Self {
            store: store.clone(),
            library_read_shutdown: Arc::new(LibraryReadShutdown(library_reads.clone())),
            library_reads,
            app_reader,
            password_login: Arc::new(crate::password_login::PasswordLogin::new(
                Arc::new(crate::password_login::Argon2Engine),
                2,
            )),
            http_work: Arc::new(crate::blocking_budget::BlockingBudget::new(8)),
            search_work: Arc::new(crate::blocking_budget::BlockingBudget::new(4)),
            fs_work: Arc::new(crate::blocking_budget::BlockingBudget::new(2)),
            browser_reconcile: Default::default(),
            backdrop_enrichment: Arc::new(crate::backdrop_enrichment::BackdropEnrichment::new(2)),
            notify_transport: Arc::new(crate::http::notify::notify_transport::HttpNotifyTransport),
            subtitle_delivery: Arc::new(crate::subtitle_delivery::SubtitleDelivery::new(
                Arc::new(library::ControlledSubtitleRunner),
                2,
            )),
            file_stats: Arc::new(crate::file_stat_read::LocalFileStatRead),
            library_files: Arc::new(crate::library_presence::SystemLibraryFileRead),
            chapter_probe: Arc::new(crate::marker_resolver::FfprobeChapters),
            playback_clock: Arc::new(crate::playback_events::SystemPlaybackClock),
            job_updates: Arc::new(crate::job_updates::JobUpdates::new()),
            cover_service: Arc::new(crate::library_cover_service::LibraryCoverService::local()),
            scan_coordinator: Arc::new(crate::scan_operations::ScanCoordinator::new()),
            library_work,
            jobs: Arc::new(Mutex::new(jobs)),
            job_loop_control: Arc::new(crate::job_loop::JobLoopControl::new()),
            instance_writable: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            write_gate: Arc::new(crate::write_gate::WriteGate::new()),
            maintenance_executor: Arc::new(crate::maintenance::SqliteExecutor),
            maintenance_lock: Arc::new(Mutex::new(())),
            active_job_attempts: Arc::new(Mutex::new(HashMap::new())),
            subscribe_guards: Arc::new(Mutex::new(HashMap::new())),
            deleting_subscribes: Arc::new(Mutex::new(HashSet::new())),
            jellyfin_server_id: Arc::new(jellyfin_server_id),
            indexer: Arc::new(Indexer::new(profiles, fetcher)),
            downloader,
            library_root: Arc::new(library_root),
            catalog: catalog.clone(),
            scan_catalog: Arc::new(Mutex::new((catalog, media_posters.clone()))),
            poster_fetch: Arc::new(HttpPoster),
            fanart_cache: Arc::default(),
            media_posters,
            log_buffer: LogBuffer::new(DEFAULT_CAPACITY),
            obscura: Arc::new(crate::obscura_manager::ObscuraManager::new(&data_dir)),
            browser: Arc::new(startup_browser.0),
            probe,
            chapter_frames,
            jellyfin_candidates: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            jellyfin_heavy_items: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            jellyfin_hydrated_rows: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            query_phase_totals: Arc::new(QueryPhaseTotals::default()),
            protocol_selection_barrier: None,
            library_header_barrier: None,
            library_read_stage_barrier: None,
            browse_page_decoded_files: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            browse_page_decoded_media: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            browse_page_candidate_rows: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            browse_page_dependency_rows: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            statistics_phase_barrier: None,
            statistics_role_lookups: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        state.finish_initialization(startup_browser.1)
    }

    pub fn with_log_buffer(mut self, log_buffer: LogBuffer) -> Self {
        self.log_buffer = log_buffer;
        self
    }
    pub fn with_library_file_read(
        mut self,
        files: Arc<dyn crate::library_presence::LibraryFileRead>,
    ) -> Self {
        self.library_files = files;
        self
    }
    /// Replace the chapter frame renderer (tests inject a gated fake; no ffmpeg).
    pub fn with_chapter_frame_runner(
        mut self,
        runner: Arc<dyn crate::chapter_frame_service::ChapterFrameRunner>,
    ) -> Self {
        let service = Arc::new(
            crate::chapter_frame_service::ChapterFrameService::with_runner(
                self.store.clone(),
                self.library_work.clone(),
                runner,
            ),
        );
        self.chapter_frames.abort();
        self.probe.set_chapter_frames(service.clone());
        self.chapter_frames = service;
        self
    }

    pub fn chapter_frames(&self) -> Arc<crate::chapter_frame_service::ChapterFrameService> {
        self.chapter_frames.clone()
    }
    pub fn with_playback_clock(
        mut self,
        clock: Arc<dyn crate::playback_events::PlaybackClock>,
    ) -> Self {
        self.playback_clock = clock;
        self
    }

    /// 替换密码登录服务（测试注入阻塞/观测引擎；生产默认 Argon2、并发 2）。
    pub fn with_password_login(
        mut self,
        service: Arc<crate::password_login::PasswordLogin>,
    ) -> Self {
        self.password_login = service;
        self
    }

    /// 替换 blocking 预算（测试注入小并发以观察准入/超时行为）。
    pub fn with_http_work(mut self, budget: Arc<crate::blocking_budget::BlockingBudget>) -> Self {
        self.http_work = budget;
        self
    }

    /// A one-reader fixture makes pool saturation observable through the Router.
    pub fn with_library_read_pool_size(mut self, size: NonZeroUsize) -> Result<Self, StoreError> {
        let pool = Arc::new(self.store.lock().open_library_read_pool(size)?);
        self.library_reads = pool.clone();
        self.library_read_shutdown = Arc::new(LibraryReadShutdown(pool));
        Ok(self)
    }

    #[doc(hidden)]
    pub fn with_library_read_progress_observer(
        self,
        observer: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.library_reads
            .set_query_progress_observer(Some(observer));
        self
    }

    pub fn close_library_reads(&self) {
        self.library_reads.close();
    }

    #[doc(hidden)]
    pub fn library_read_pool_weak(&self) -> Weak<store::LibraryReadPool> {
        Arc::downgrade(&self.library_reads)
    }

    pub fn with_browser(mut self, browser: Arc<indexer::Browser>) -> Self {
        self.browser = browser;
        self
    }

    /// 替换浏览文件访问 seam（测试注入阻塞实现以证明 NFO/stat 不在 Store 锁内）。
    pub fn with_file_stat_read(
        mut self,
        files: Arc<dyn crate::file_stat_read::FileStatRead>,
    ) -> Self {
        self.file_stats = files;
        self
    }

    /// 替换 Jellyfin 选择计数 seam（测试注入自有计数器以观察候选/重元数据条数）。
    pub fn with_jellyfin_work_counters(
        mut self,
        candidates: Arc<std::sync::atomic::AtomicUsize>,
        heavy_items: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        self.jellyfin_candidates = candidates;
        self.jellyfin_heavy_items = heavy_items;
        self
    }

    /// Jellyfin normal pages' selected references (NextUp keeps its own candidate count).
    pub fn jellyfin_candidates(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.jellyfin_candidates.clone()
    }

    /// Jellyfin 选择构建过重元数据的条目数。
    pub fn jellyfin_heavy_items(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.jellyfin_heavy_items.clone()
    }

    pub fn jellyfin_hydrated_rows(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.jellyfin_hydrated_rows.clone()
    }

    pub fn query_phase_snapshot(&self) -> QueryPhaseSnapshot {
        self.query_phase_totals.snapshot()
    }

    pub(crate) fn record_query_phase(&self, phase: &'static str, duration: Duration) {
        self.query_phase_totals.record(phase, duration);
    }

    pub fn with_protocol_selection_barrier(
        mut self,
        barrier: Arc<dyn Fn(&domain::ProtocolItemPage) + Send + Sync>,
    ) -> Self {
        self.protocol_selection_barrier = Some(barrier);
        self
    }

    #[doc(hidden)]
    pub fn with_library_header_barrier(mut self, barrier: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.library_header_barrier = Some(barrier);
        self
    }

    #[doc(hidden)]
    pub fn with_library_read_stage_barrier(
        mut self,
        barrier: Arc<dyn Fn(&'static str) + Send + Sync>,
    ) -> Self {
        self.library_read_stage_barrier = Some(barrier);
        self
    }

    /// 替换浏览页工作量计数 seam（测试注入自有计数器以观察本页解码行数）。
    pub fn with_browse_page_counters(
        mut self,
        decoded_files: Arc<std::sync::atomic::AtomicUsize>,
        decoded_media: Arc<std::sync::atomic::AtomicUsize>,
        candidate_rows: Arc<std::sync::atomic::AtomicUsize>,
        dependency_rows: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        self.browse_page_decoded_files = decoded_files;
        self.browse_page_decoded_media = decoded_media;
        self.browse_page_candidate_rows = candidate_rows;
        self.browse_page_dependency_rows = dependency_rows;
        self
    }

    /// 浏览请求真正解码的投影 file facts 行数。
    pub fn browse_page_decoded_files(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.browse_page_decoded_files.clone()
    }

    /// 浏览请求真正解码的投影 media facts 行数。
    pub fn browse_page_decoded_media(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.browse_page_decoded_media.clone()
    }

    /// 选页读过的轻候选 JSON 行数。
    pub fn browse_page_candidate_rows(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.browse_page_candidate_rows.clone()
    }

    /// 投影依赖覆盖读过的轻量行数。
    pub fn browse_page_dependency_rows(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.browse_page_dependency_rows.clone()
    }

    /// 记录本次请求真正解码的投影行数：计数在**读取/解析处**，包含随后被丢弃或
    /// 解析失败的行（测试 seam；生产只读计数，无副作用）。
    #[allow(dead_code)]
    pub(crate) fn count_decoded(&self, files: usize, media: usize) {
        self.browse_page_decoded_files
            .fetch_add(files, std::sync::atomic::Ordering::SeqCst);
        self.browse_page_decoded_media
            .fetch_add(media, std::sync::atomic::Ordering::SeqCst);
    }

    /// 记录选页读过的轻候选 JSON 行数。
    #[allow(dead_code)]
    pub(crate) fn count_candidate_rows(&self, rows: usize) {
        self.browse_page_candidate_rows
            .fetch_add(rows, std::sync::atomic::Ordering::SeqCst);
    }

    /// 记录依赖覆盖读过的轻量行数（file 依赖列 + 候选行）。
    #[allow(dead_code)]
    pub(crate) fn count_dependency_rows(&self, rows: usize) {
        self.browse_page_dependency_rows
            .fetch_add(rows, std::sync::atomic::Ordering::SeqCst);
    }

    /// 替换 notify 发送实现（测试注入阻塞实现以验证预算准入）。
    pub fn with_notify_transport(mut self, transport: Arc<dyn crate::NotifyTransport>) -> Self {
        self.notify_transport = transport;
        self
    }

    /// 替换字幕交付服务（测试注入 FakeRunner 与并发上限）。
    pub fn with_subtitle_delivery(
        mut self,
        service: Arc<crate::subtitle_delivery::SubtitleDelivery>,
    ) -> Self {
        self.subtitle_delivery = service;
        self
    }

    /// 替换任务计数 watcher（测试注入以观察共享 poller 生命周期）。
    pub fn with_job_updates(mut self, updates: Arc<crate::job_updates::JobUpdates>) -> Self {
        self.job_updates = updates;
        self
    }

    /// 封面服务（渲染 + revision CAS 发布）。
    pub fn cover_service(&self) -> &Arc<crate::library_cover_service::LibraryCoverService> {
        &self.cover_service
    }

    /// 替换封面服务（测试注入 gate 渲染器以验证 CAS 发布协议）。
    pub fn with_cover_service(
        mut self,
        service: Arc<crate::library_cover_service::LibraryCoverService>,
    ) -> Self {
        self.cover_service = service;
        self
    }

    pub fn jellyfin_server_id(&self) -> &str {
        self.jellyfin_server_id.as_str()
    }

    pub fn store(&self) -> Arc<Mutex<Store>> {
        self.store.clone()
    }

    pub fn jobs(&self) -> Arc<Mutex<jobs::Queue>> {
        self.jobs.clone()
    }

    /// H-1: whether this instance currently owns the data directory.
    pub fn instance_writable(&self) -> bool {
        self.instance_writable
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn with_instance_writable(self, writable: bool) -> Self {
        self.instance_writable
            .store(writable, std::sync::atomic::Ordering::SeqCst);
        self
    }

    /// H-1: 409 for write endpoints while another instance holds the lease.
    pub(crate) fn require_writable(&self) -> Result<(), axum::response::Response> {
        if self.instance_writable() {
            return Ok(());
        }
        Err(crate::http::err(
            axum::http::StatusCode::CONFLICT,
            "instance.read_only",
            "数据目录由另一个实例占用，本实例为只读模式",
        ))
    }

    /// L5: 维护期写门禁（维护任务关闸、前台写入口 `admit()`）。
    pub fn write_gate(&self) -> Arc<crate::write_gate::WriteGate> {
        self.write_gate.clone()
    }

    /// L5: 维护关闸期间的写请求 → 503 + `Retry-After`。
    ///
    /// 与只读的 409（`require_writable`）是两个原因，状态码不能合并：
    /// 只读说明「另一个实例在写这个数据目录」，503 说明「本实例正在维护，稍后重试」。
    pub(crate) fn require_write_admitted(&self) -> Result<(), axum::response::Response> {
        self.write_gate
            .admit()
            .map_err(crate::write_gate::busy_response)
    }

    pub fn job_loop_control(&self) -> Arc<crate::job_loop::JobLoopControl> {
        self.job_loop_control.clone()
    }

    /// 替换数据库维护执行器（测试注入阻塞/报 busy/磁盘不足的假实现）。
    pub fn with_maintenance_executor(
        mut self,
        executor: Arc<dyn crate::maintenance::DatabaseExecutor>,
    ) -> Self {
        self.maintenance_executor = executor;
        self
    }

    pub fn with_job_loop_control(mut self, control: Arc<crate::job_loop::JobLoopControl>) -> Self {
        self.job_loop_control = control;
        self
    }

    /// Library 工作注册表：测试用它取得/释放 lease 以观察重型维护避让。
    pub fn library_work(&self) -> Arc<crate::library_lifecycle::LibraryWorkRegistry> {
        self.library_work.clone()
    }

    pub fn subscribe_guard(&self, id: SubscribeId) -> Arc<Mutex<()>> {
        let mut guards = self.subscribe_guards.lock();
        guards.retain(|_, guard| guard.strong_count() > 0);
        if let Some(guard) = guards.get(&id).and_then(Weak::upgrade) {
            return guard;
        }
        let guard = Arc::new(Mutex::new(()));
        guards.insert(id, Arc::downgrade(&guard));
        guard
    }

    /// Deletion state is read and written while holding `subscribe_guard(id)`.
    pub fn mark_subscribe_deleting(&self, id: SubscribeId) {
        self.deleting_subscribes.lock().insert(id);
    }

    pub fn subscribe_is_deleting(&self, id: SubscribeId) -> bool {
        self.deleting_subscribes.lock().contains(&id)
    }

    pub fn clear_subscribe_deleting(&self, id: SubscribeId) {
        self.deleting_subscribes.lock().remove(&id);
    }

    pub fn with_poster_fetch(mut self, poster_fetch: Arc<dyn PosterFetch>) -> Self {
        self.poster_fetch = poster_fetch;
        self
    }
}

/// Keeps a subscription marked as deleting across async cleanup awaits. Drop
/// clears the reservation after the caller has either deleted the row or
/// abandoned cleanup, so a failed/cancelled request does not strand it.
pub(crate) struct SubscribeDeletionReservation {
    state: ApiState,
    id: SubscribeId,
}

impl SubscribeDeletionReservation {
    pub(crate) fn new(state: ApiState, id: SubscribeId) -> Self {
        Self { state, id }
    }
}

impl Drop for SubscribeDeletionReservation {
    fn drop(&mut self) {
        let guard = self.state.subscribe_guard(self.id);
        let _guard = guard.lock();
        self.state.clear_subscribe_deleting(self.id);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiStateError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn open_jobs(jobs_path: &std::path::Path) -> Result<jobs::Queue, ApiStateError> {
    let jobs =
        jobs::Queue::open(jobs_path).map_err(|error| std::io::Error::other(error.to_string()))?;
    crate::jobs_api::seed_instance_defs(&jobs)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    // 维护任务默认 def：幂等注册，用户改过的开关/频率/payload 原样保留。
    crate::maintenance::seed_maintenance_defs(&jobs)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(jobs)
}
