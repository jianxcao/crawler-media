#![allow(dead_code)]

#[path = "../plugin_http_support/mod.rs"]
pub mod plugin_http_support;

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use api::{ApiState, FrozenDownloadPlan, Store};
use domain::{DownloaderId, LibraryId, MediaId, PluginOperationId, Site, SiteId, Torrent};
use downloader::{
    ClientSubmitResult, DiskSpaceInfo, DiskSpaceStatus, Downloader, DownloaderError,
    MemoryDownloader, SubmissionPurpose, SubmitOptions, SubmitPayload, TaskFileSet, TaskSnapshot,
    TorrentSource,
};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_http_support::TOKEN;
use serde_json::Value;
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, RecordDownloadOwnershipInput, UpsertBoostTaskFactInput,
};

pub struct NoopFetcher;
impl Fetcher for NoopFetcher {
    fn fetch(&self, _req: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

pub struct StubTorrentServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubTorrentServer {
    pub fn start(payload: Vec<u8>) -> Self {
        Self::start_inner(payload, 4096)
    }

    pub fn start_dynamic(size_bytes: usize) -> Self {
        Self::start_inner(Vec::new(), size_bytes)
    }

    fn start_inner(payload: Vec<u8>, dynamic_size: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 2048];
                        let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
                        let body = if payload.is_empty() {
                            let req_line = String::from_utf8_lossy(&buf[..n]);
                            let path = req_line
                                .split_whitespace()
                                .nth(1)
                                .unwrap_or("/default")
                                .trim_start_matches('/');
                            minimal_torrent_bytes(path, dynamic_size)
                        } else {
                            payload.clone()
                        };
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                        let _ = stream.write_all(&body);
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], self.port))
    }
}

impl Drop for StubTorrentServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub fn minimal_torrent_bytes(name: &str, length: usize) -> Vec<u8> {
    let info_dict = format!(
        "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        name.len()
    );
    let mut bytes = Vec::from(b"d8:announce20:https://tracker/test4:info");
    bytes.extend_from_slice(info_dict.as_bytes());
    bytes.push(b'e');
    bytes
}

pub struct BoostTestDownloader {
    inner: MemoryDownloader,
    disk: Mutex<DiskSpaceInfo>,
    pub unknown_on_submit: AtomicBool,
}

impl BoostTestDownloader {
    pub fn new(stage: PathBuf) -> Self {
        Self {
            inner: MemoryDownloader::new(stage),
            disk: Mutex::new(DiskSpaceInfo {
                status: DiskSpaceStatus::Available,
                available_bytes: Some(100_000_000_000),
                root_path: None,
            }),
            unknown_on_submit: AtomicBool::new(false),
        }
    }

    pub fn set_disk(&self, status: DiskSpaceStatus, available_bytes: Option<u64>) {
        *self.disk.lock().unwrap() = DiskSpaceInfo {
            status,
            available_bytes,
            root_path: None,
        };
    }
}

impl Downloader for BoostTestDownloader {
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
                reason: "simulated network timeout".into(),
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
    ) -> Result<downloader::ClientTaskQuery, DownloaderError> {
        self.inner.query_task_status(info_hash, ownership_tag)
    }
}

pub fn setup_state(
    dir: &Path,
    dl: Arc<BoostTestDownloader>,
    srv: Option<&StubTorrentServer>,
) -> (ApiState, SiteId, DownloaderId) {
    let store = Arc::new(Store::open(dir.join("data")).unwrap());
    let mut state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    if let Some(s) = srv {
        state = state.with_plugin_http_policy_override(
            net_guard::TargetPolicy::production().with_test_loopback_socket(s.addr()),
        );
    }
    let site_id = SiteId::new();
    state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "PT-Boost".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
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
            timezone: "UTC".into(),
        })
        .unwrap();
    let dl_id = DownloaderId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "BoostDL".into(),
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
    (state, site_id, dl_id)
}

pub fn save_boost_config(state: &ApiState, values: Value) {
    let store = state.store();
    let p = store.get_plugin("builtin.boost").unwrap().unwrap();
    let schema = store
        .get_plugin_config_summary("builtin.boost")
        .unwrap()
        .config
        .map(|c| c.schema_revision)
        .unwrap_or_else(|| "1".into());
    let mut cur = p;
    if cur.lifecycle == store::plugins::PluginLifecycle::Enabled {
        let dis = store
            .begin_plugin_lifecycle(&cur.plugin_id, cur.revision, uuid::Uuid::new_v4(), false)
            .unwrap();
        cur = store
            .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
                plugin_id: dis.plugin_id.clone(),
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
        .save_plugin_config("builtin.boost", cur.revision, &schema, &values)
        .unwrap();
    let en = store
        .begin_plugin_lifecycle(&saved.plugin_id, saved.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: en.plugin_id.clone(),
            operation_id: en.active_operation_id.unwrap(),
            revision: en.revision,
            generation: en.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
}

pub fn boost_plan(
    site_id: SiteId,
    dl_id: DownloaderId,
    url: &str,
    title: &str,
) -> FrozenDownloadPlan {
    FrozenDownloadPlan {
        torrent: Torrent {
            site_id,
            id: Some(title.into()),
            title: title.into(),
            enclosure: url.into(),
            size_bytes: None,
            seeders: Some(5),
            leechers: Some(1),
            snatched: None,
            upload_time: None,
            poster_url: None,
            free: true,
            hr: Some(false),
            imdb_id: None,
            detail_url: None,
            category: None,
        },
        source: TorrentSource::from_url(url).unwrap(),
        purpose: SubmissionPurpose::Boost,
        downloader_id: Some(dl_id),
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: None,
        release_override: None,
        force: false,
        save_path: None,
        category: None,
        tags: vec!["plugin:builtin.boost:auto".into()],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: Some(format!("builtin.boost:{dl_id}")),
        plan_digest: None,
    }
}

pub fn boost_ctx(key: &str) -> api::DownloadOpContext {
    api::DownloadOpContext {
        operation_id: PluginOperationId::new(),
        cause_id: format!("cause-{key}"),
        subject: "builtin.boost".into(),
        idempotency_key: key.into(),
        plugin_id: Some("builtin.boost".into()),
        generation: Some(1),
        config_revision: None,
        dry_run: false,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn seed_boost_task(
    store: &Store,
    dl_id: DownloaderId,
    site_id: SiteId,
    tag: &str,
    hash: &str,
    state_val: BoostTaskState,
    occupied_bytes: i64,
    retained_bytes: i64,
    hr_active: Option<bool>,
    hold_until: Option<i64>,
) -> String {
    let op_id = format!("op-{tag}");
    let now = 1_700_000_000i64;
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{tag}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "builtin.boost".into(),
            cause_id: "seed".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            bytes_digest: Some(format!("dig-{tag}")),
            verified_bytes: Some(occupied_bytes.max(retained_bytes)),
            request_digest: format!("req-{tag}"),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "builtin.boost".into(),
            site_id: Some(site_id),
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(occupied_bytes.max(retained_bytes)),
            bytes_digest: Some(format!("dig-{tag}")),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: format!("tf-{tag}"),
            plugin_id: "builtin.boost".into(),
            quota_key: format!("builtin.boost:{dl_id}"),
            downloader_id: dl_id,
            site_id: Some(site_id),
            ownership_tag: tag.into(),
            info_hash: hash.into(),
            state: state_val,
            occupied_bytes,
            retained_bytes,
            hr_active,
            hr_until: None,
            hold_until,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap()
        .task_fact_id
}

pub async fn poll_op(env: &plugin_http_support::HttpEnv, op_id: &str) -> Value {
    for _ in 0..80 {
        let _ = env
            .send(plugin_http_support::req_json(
                "POST",
                "/api/v1/jobs/tick",
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let (st, _, b) = env
            .send(plugin_http_support::req_json(
                "GET",
                &format!("/api/v1/plugin-operations/{op_id}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let op = plugin_http_support::assert_ok(st, &b, axum::http::StatusCode::OK);
        if matches!(
            op["state"].as_str(),
            Some("succeeded" | "rejected" | "failed" | "cancelled")
        ) {
            return op;
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
    panic!("plugin operation {op_id} timed out")
}

pub fn seed_library_file_entry(store: &Store, path: &Path) {
    let lib_id = LibraryId::new();
    let root_id = uuid::Uuid::new_v4().to_string();
    let media_id = MediaId::new();
    let path_str = path.to_string_lossy().into_owned();
    let root_dir = path.parent().unwrap().to_string_lossy().into_owned();
    store
        .with_writer(store::Db::Library, |conn| {
            conn.execute(
                "INSERT INTO media (media_id, kind, title, identity_state) VALUES (?1, 'movie', 'KeepMe', 'confirmed')",
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
                 VALUES (?1, ?2, ?3, ?4, 'movie', ?5, 'InLibrary.mkv', 'present', 'published', 1, 'v1', ?5, 'release', 'high')",
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
