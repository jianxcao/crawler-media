#[path = "../builtin_boost/support.rs"]
pub mod base_support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use base_support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use base_support::{NoopFetcher, StubTorrentServer, poll_op};
use domain::{DownloaderId, LibraryId, MediaId, Site, SiteId, Torrent};
use downloader::{
    ClientSubmitResult, ClientTaskQuery, DiskSpaceInfo, DiskSpaceStatus, Downloader,
    DownloaderError, MemoryDownloader, MemoryTaskMetrics, SubmitOptions, SubmitPayload,
    TaskFileSet, TaskSnapshot,
};
use indexer::{IndexerError, ProviderCapability, RegistrySnapshot, UserStats};
use serde_json::{Value, json};
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycle, PluginLifecycleObservation,
    PluginVersionMetadata,
};
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, RecordDownloadOwnershipInput, UpsertBoostTaskFactInput,
};

pub const BRUSH_PLUGIN_ID: &str = "org.example.brush";

pub struct BrushTestDownloader {
    pub inner: MemoryDownloader,
    disk: StdMutex<DiskSpaceInfo>,
    pub unknown_on_submit: AtomicBool,
}

impl BrushTestDownloader {
    pub fn new(stage: PathBuf) -> Self {
        Self {
            inner: MemoryDownloader::new(stage),
            disk: StdMutex::new(DiskSpaceInfo {
                status: DiskSpaceStatus::Available,
                available_bytes: Some(100_000_000_000),
                root_path: None,
            }),
            unknown_on_submit: AtomicBool::new(false),
        }
    }

    pub fn submitted_options(&self) -> Vec<SubmitOptions> {
        self.inner
            .submitted()
            .into_iter()
            .map(|s| s.options)
            .collect()
    }

    pub fn set_task_files(&self, info_hash: &str, files: Vec<PathBuf>, total_bytes: u64) {
        self.inner.set_task_files(info_hash, files, total_bytes);
    }

    pub fn set_task_metrics(&self, info_hash: &str, metrics: MemoryTaskMetrics) {
        self.inner.set_task_metrics(info_hash, metrics);
    }
}

impl Downloader for BrushTestDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(torrent, hash)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if self.unknown_on_submit.load(Ordering::SeqCst) {
            return Ok(ClientSubmitResult::UnknownOutcome {
                reason: "simulated unknown".into(),
            });
        }
        self.inner.submit_verified(payload, options)
    }
    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(torrent)
    }
    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(info_hash, delete_files)
    }
    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        self.inner.pause_task(info_hash)
    }
    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        self.inner.resume_task(info_hash)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        self.inner.task_snapshots()
    }
    fn task_files_by_hash(&self, info_hash: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        self.inner.task_files_by_hash(info_hash)
    }
    fn set_task_tags(&self, info_hash: &str, tags: &[String]) -> Result<(), DownloaderError> {
        self.inner.set_task_tags(info_hash, tags)
    }
    fn set_task_category(&self, info_hash: &str, category: &str) -> Result<(), DownloaderError> {
        self.inner.set_task_category(info_hash, category)
    }
    fn set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        self.inner
            .set_task_limits(info_hash, upload_limit, download_limit)
    }
    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        Ok(self.disk.lock().unwrap().clone())
    }
    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        self.inner.query_task_status(info_hash, ownership_tag)
    }
}

#[derive(Clone, Default)]
pub struct SiteFeedData {
    pub search_hits: Vec<Torrent>,
    pub rss_hits: Vec<Torrent>,
    pub stats: Option<(u64, u64)>,
}

pub type CapturedSearchCategories = Arc<StdMutex<Vec<(SiteId, Vec<String>)>>>;

pub struct StubBrushProvider {
    pub feeds: Arc<StdMutex<HashMap<SiteId, SiteFeedData>>>,
    pub captured_search_categories: CapturedSearchCategories,
    pub desc: indexer::ProviderDescriptor,
}

impl StubBrushProvider {
    pub fn new(id: &str, caps: Vec<ProviderCapability>) -> Self {
        Self {
            feeds: Arc::new(StdMutex::new(HashMap::new())),
            captured_search_categories: Arc::new(StdMutex::new(Vec::new())),
            desc: indexer::ProviderDescriptor {
                id: id.into(),
                name: format!("Stub:{id}"),
                kind: "builtin".into(),
                source: indexer::ProviderSource::Builtin,
                auth_types: vec![indexer::AuthType::Cookie],
                capabilities: caps,
                availability: indexer::ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
        }
    }
}

impl indexer::IndexerProvider for StubBrushProvider {
    fn descriptor(&self) -> &indexer::ProviderDescriptor {
        &self.desc
    }
    fn search(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &Site,
        query: &indexer::SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.captured_search_categories
            .lock()
            .unwrap()
            .push((site.id, query.categories.to_vec()));
        Ok(self
            .feeds
            .lock()
            .unwrap()
            .get(&site.id)
            .map(|d| d.search_hits.clone())
            .unwrap_or_default())
    }
    fn rss(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(self
            .feeds
            .lock()
            .unwrap()
            .get(&site.id)
            .map(|d| d.rss_hits.clone())
            .unwrap_or_default())
    }
    fn user_stats(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &Site,
    ) -> Result<UserStats, IndexerError> {
        let stats = self
            .feeds
            .lock()
            .unwrap()
            .get(&site.id)
            .and_then(|d| d.stats);
        Ok(UserStats {
            uploaded: stats.map(|s| s.0),
            downloaded: stats.map(|s| s.1),
        })
    }
    fn resolve_download(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

pub fn make_torrent(site_id: SiteId, url: &str, title: &str) -> Torrent {
    Torrent {
        site_id,
        id: Some(title.into()),
        title: title.into(),
        enclosure: url.into(),
        size_bytes: Some(4096),
        seeders: Some(10),
        leechers: Some(2),
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: Some("tv".into()),
        free_2x: false,
        free_until: None,
    }
}

pub fn install_brush_plugin(state: &ApiState, plugin_id: &str) {
    let p = state
        .store()
        .register_plugin_version(
            &PluginVersionMetadata {
                plugin_id: plugin_id.into(),
                package_hash: format!("{plugin_id}:h1"),
                version: "1.0.0".into(),
                manifest_json: "{}".into(),
                config_schema_json: "{}".into(),
                state_schema: "1".into(),
                artifact_path: "unused".into(),
                signing_key_id: None,
                signature_status: "development".into(),
                ui_metadata_ref: None,
                credential_bound: false,
            },
            0,
        )
        .unwrap();
    let grants: Vec<PluginGrant> = [
        "kv",
        "read:sites",
        "read:downloads",
        "read:boost",
        "command:boost.run",
        "command:indexer.search",
        "command:indexer.rss",
        "command:downloader.add",
        "command:downloader.control",
        "command:downloader.delete-task",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
    ]
    .into_iter()
    .map(|cap| PluginGrant {
        capability: cap.into(),
        scope: PluginGrantScope::Global,
        sensitive: cap == "command:downloader.delete-files",
    })
    .collect();
    let p = state
        .store()
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &grants)
        .unwrap();
    let dis = state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    state
        .store()
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: dis.plugin_id,
            operation_id: dis.active_operation_id.unwrap(),
            revision: dis.revision,
            generation: dis.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
}

pub fn save_plugin_cfg(state: &ApiState, plugin_id: &str, values: Value) {
    let store = state.store();
    let mut cur = store.get_plugin(plugin_id).unwrap().unwrap();
    if cur.lifecycle == PluginLifecycle::Enabled {
        let dis = store
            .begin_plugin_lifecycle(&cur.plugin_id, cur.revision, uuid::Uuid::new_v4(), false)
            .unwrap();
        cur = store
            .finalize_plugin_lifecycle(&PluginLifecycleObservation {
                plugin_id: dis.plugin_id,
                operation_id: dis.active_operation_id.unwrap(),
                revision: dis.revision,
                generation: dis.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            })
            .unwrap();
    }
    let saved = store
        .save_plugin_config(plugin_id, cur.revision, "1", &values)
        .unwrap();
    let en = store
        .begin_plugin_lifecycle(&saved.plugin_id, saved.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: en.plugin_id,
            operation_id: en.active_operation_id.unwrap(),
            revision: en.revision,
            generation: en.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
}

pub struct BrushEnv {
    pub http: HttpEnv,
    pub dl: Arc<BrushTestDownloader>,
    pub dl_id: DownloaderId,
    pub site_search: SiteId,
    pub site_rss: SiteId,
    pub search_provider: Arc<StubBrushProvider>,
    pub rss_provider: Arc<StubBrushProvider>,
    pub srv: StubTorrentServer,
}

impl BrushEnv {
    pub fn new(dynamic_size: usize) -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let srv = StubTorrentServer::start_dynamic(dynamic_size);
        let dl = Arc::new(BrushTestDownloader::new(dir.path().join("stage")));
        let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
        let search_provider = Arc::new(StubBrushProvider::new(
            "brush_search_idx",
            vec![ProviderCapability::Search, ProviderCapability::UserStats],
        ));
        let rss_provider = Arc::new(StubBrushProvider::new(
            "brush_rss_idx",
            vec![ProviderCapability::Rss, ProviderCapability::UserStats],
        ));
        let reg = RegistryCandidate::from_builtin()
            .unwrap()
            .add_indexer_provider(search_provider.clone())
            .unwrap()
            .add_indexer_provider(rss_provider.clone())
            .unwrap();
        let state = ApiState::new_arc(
            store,
            TOKEN.into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoopFetcher),
            dl.clone(),
        )
        .unwrap()
        .with_plugin_http_policy_override(
            net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr()),
        );
        state.indexer().publish(reg).unwrap();

        let site_search = insert_site(&state, "BrushSearch", "brush_search_idx", "UTC");
        let site_rss = insert_site(&state, "BrushRss", "brush_rss_idx", "UTC");
        let dl_id = DownloaderId::new();
        state
            .store()
            .insert_downloader(&store::DownloaderRow {
                id: dl_id,
                name: "BrushDL".into(),
                kind: "qbittorrent".into(),
                url: "http://127.0.0.1:9".into(),
                username: None,
                password: None,
                api_key: None,
                category: None,
                path_maps: vec![],
                is_default: true,
                enabled: true,
            })
            .unwrap();
        install_brush_plugin(&state, BRUSH_PLUGIN_ID);
        let http = HttpEnv {
            _dir: dir,
            app: router(state.clone()),
            state,
            member_token: String::new(),
        };
        Self {
            http,
            dl,
            dl_id,
            site_search,
            site_rss,
            search_provider,
            rss_provider,
            srv,
        }
    }

    pub fn stage_dir(&self) -> PathBuf {
        self.http._dir.path().join("stage")
    }

    pub async fn run_cycle(&self, payload: Value, idem: &str) -> Value {
        let rev = self
            .http
            .state
            .store()
            .get_plugin(BRUSH_PLUGIN_ID)
            .unwrap()
            .unwrap()
            .revision;
        let (st, _, body) = self
            .http
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{BRUSH_PLUGIN_ID}/commands"),
                Some(TOKEN),
                &[
                    ("If-Match", &format!("\"{rev}\"")),
                    ("Idempotency-Key", idem),
                ],
                Some(json!({ "kind": "boost.run", "payload": payload })),
            ))
            .await;
        let accepted = assert_ok(st, &body, StatusCode::ACCEPTED);
        let op = poll_op(&self.http, accepted["operation_id"].as_str().unwrap()).await;
        assert_eq!(op["state"], "succeeded", "boost.run failed: {op}");
        op["result"].clone()
    }
}

use indexer::RegistryCandidate;

pub fn insert_site(state: &ApiState, name: &str, profile_id: &str, timezone: &str) -> SiteId {
    let site_id = SiteId::new();
    state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: name.into(),
            url: "https://pt.example/".into(),
            profile_id: profile_id.into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: Some(60),
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: timezone.into(),
        })
        .unwrap();
    site_id
}

#[allow(clippy::too_many_arguments)]
pub fn seed_task_with_live(
    env: &BrushEnv,
    site_id: SiteId,
    tag: &str,
    name: &str,
    size: i64,
    state_val: BoostTaskState,
    hr_active: Option<bool>,
    hold_until: Option<i64>,
    free_until: Option<i64>,
    protection_state: BoostProtectionState,
    protection_reason: Option<&str>,
    metrics: MemoryTaskMetrics,
) -> (String, String) {
    let bytes = base_support::minimal_torrent_bytes(name, size as usize);
    let hash = downloader::torrent_info_hash(&bytes).unwrap();
    let payload = SubmitPayload::TorrentBytes {
        bytes,
        bytes_digest: format!("dig-{tag}"),
        info_hash: hash.clone(),
    };
    let opts = SubmitOptions {
        ownership_tag: tag.into(),
        save_path: None,
        category: Some("boost".into()),
        extra_tags: vec![],
        paused: state_val == BoostTaskState::Paused,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    env.dl.submit_verified(&payload, &opts).unwrap();
    env.dl.set_task_metrics(&hash, metrics);

    let now = api::job_loop::unix_now();
    let op_id = format!("op-{tag}");
    env.http
        .state
        .store()
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{tag}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(BRUSH_PLUGIN_ID.into()),
            subject: BRUSH_PLUGIN_ID.into(),
            cause_id: "seed".into(),
            site_id: Some(site_id),
            downloader_id: Some(env.dl_id),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_tag: tag.into(),
            info_hash: Some(hash.clone()),
            bytes_digest: Some(format!("dig-{tag}")),
            verified_bytes: Some(size),
            request_digest: format!("req-{tag}"),
            now_utc: now,
        })
        .unwrap();
    env.http
        .state
        .store()
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: env.dl_id,
            ownership_tag: tag.into(),
            info_hash: Some(hash.clone()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some(BRUSH_PLUGIN_ID.into()),
            subject: BRUSH_PLUGIN_ID.into(),
            site_id: Some(site_id),
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(size),
            bytes_digest: Some(format!("dig-{tag}")),
            now_utc: now,
        })
        .unwrap();
    let tf = env
        .http
        .state
        .store()
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: format!("tf-{tag}"),
            plugin_id: BRUSH_PLUGIN_ID.into(),
            quota_key: format!("{BRUSH_PLUGIN_ID}:{}", env.dl_id),
            downloader_id: env.dl_id,
            ownership_tag: tag.into(),
            info_hash: hash.clone(),
            site_id: Some(site_id),
            state: state_val,
            occupied_bytes: size,
            retained_bytes: 0,
            hr_active,
            hr_until: None,
            hold_until,
            free_until,
            protection_state,
            protection_reason: protection_reason.map(str::to_string),
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap();
    (tf.task_fact_id, hash)
}

pub fn seed_library_path(store: &Store, path: &Path) {
    let lib_id = LibraryId::new();
    let root_id = uuid::Uuid::new_v4().to_string();
    let media_id = MediaId::new();
    let path_str = path.to_string_lossy().into_owned();
    let root_dir = path.parent().unwrap().to_string_lossy().into_owned();
    store
        .with_writer(store::Db::Library, |conn| {
            conn.execute(
                "INSERT INTO media (media_id, kind, title, identity_state) VALUES (?1, 'movie', 'LibKeep', 'confirmed')",
                [media_id.to_string()],
            )?;
            conn.execute(
                "INSERT INTO libraries (library_id, kind, name) VALUES (?1, 'movie', 'Movies')",
                [lib_id.to_string()],
            )?;
            conn.execute(
                "INSERT INTO library_roots (root_id, library_id, path_key, display_path) VALUES (?1, ?2, ?3, ?3)",
                rusqlite::params![root_id, lib_id.to_string(), root_dir],
            )?;
            conn.execute(
                "INSERT INTO library_files (file_id, library_id, root_id, media_id, kind, path_key, display_path, presence_state, publication_state, file_revision, source_version, source_path, quality_source, confidence)
                 VALUES (?1, ?2, ?3, ?4, 'movie', ?5, 'LibKeep.mkv', 'present', 'published', 1, 'v1', ?5, 'release', 'high')",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    lib_id.to_string(),
                    root_id,
                    media_id.to_string(),
                    path_str,
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn seed_transfer_active_hash(store: &Store, info_hash: &str) {
    let sub_id = uuid::Uuid::new_v4().to_string();
    store
        .with_writer(store::Db::Subscribe, |conn| {
            conn.execute(
                "INSERT INTO subscribes (id, user_id, media_id, coverage_kind, fetch_mode, filter_id, created_at, updated_at, jobs_sync_generation, lifecycle_epoch)
                 VALUES (?1, 'u1', 'm1', 'movie', 'auto', 'f1', 0, 0, 'g1', 1)",
                [sub_id.as_str()],
            )?;
            conn.execute(
                "INSERT INTO pending_downloads (subscribe_id, enclosure, title, score, torrent_json, submitted_at, state, info_hash)
                 VALUES (?1, ?2, 'ActiveTransfer', 1, '{}', 1, 'active', ?3)",
                rusqlite::params![sub_id, format!("https://pt.example/{info_hash}.torrent"), info_hash],
            )?;
            Ok(())
        })
        .unwrap();
}
