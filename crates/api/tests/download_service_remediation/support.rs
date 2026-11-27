#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use api::{ApiState, DownloadOpContext, FrozenDownloadPlan, Store, SubmissionPurpose};
use domain::{DownloaderId, PluginOperationId, Site, SiteId, Torrent};
use downloader::{
    ClientSubmitResult, ClientTaskQuery, Downloader, DownloaderError, MemoryDownloader,
    SubmitOptions, SubmitPayload, TaskSnapshot,
};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_api::HOOK_WORLD;

use super::plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, pkg};

pub const POLICY_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");

pub struct NoopFetcher;
impl Fetcher for NoopFetcher {
    fn fetch(&self, _req: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

pub fn minimal_torrent_bytes(name: &str, length: usize) -> (Vec<u8>, String) {
    let info_dict = format!(
        "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        name.len()
    );
    let mut bytes = Vec::from(b"d8:announce20:https://tracker/test4:info");
    bytes.extend_from_slice(info_dict.as_bytes());
    bytes.push(b'e');
    let hash = downloader::torrent_info_hash(&bytes).unwrap();
    (bytes, hash)
}

pub struct StubHttpServer {
    pub addr: SocketAddr,
    pub status: Arc<Mutex<u16>>,
    pub redirect_to: Arc<Mutex<Option<String>>>,
    pub captured_requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubHttpServer {
    pub fn start(payload: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(200u16));
        let redirect_to: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_requests = Arc::new(Mutex::new(Vec::new()));
        let (stop_c, status_c, redir_c, cap_c) = (
            Arc::clone(&stop),
            Arc::clone(&status),
            Arc::clone(&redirect_to),
            Arc::clone(&captured_requests),
        );
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        cap_c
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                        if let Some(loc) = redir_c.lock().unwrap().clone() {
                            let hdr = format!(
                                "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.flush();
                            continue;
                        }
                        let code = *status_c.lock().unwrap();
                        if code != 200 {
                            let hdr = format!(
                                "HTTP/1.1 {code} ERR\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.flush();
                            continue;
                        }
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                        let _ = stream.write_all(&payload);
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
            addr,
            status,
            redirect_to,
            captured_requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.addr.port())
    }
}

impl Drop for StubHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct RecordingDownloader {
    pub inner: MemoryDownloader,
    pub submit_count: AtomicUsize,
    pub unknown_once: AtomicBool,
    pub on_submit: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl RecordingDownloader {
    pub fn new(stage: PathBuf) -> Self {
        Self {
            inner: MemoryDownloader::new(stage),
            submit_count: AtomicUsize::new(0),
            unknown_once: AtomicBool::new(false),
            on_submit: None,
        }
    }

    pub fn with_on_submit(mut self, f: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_submit = Some(f);
        self
    }
}

impl Downloader for RecordingDownloader {
    fn remember_identity(&self, t: &Torrent, h: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(t, h)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if let Some(cb) = &self.on_submit {
            cb();
        }
        self.submit_count.fetch_add(1, Ordering::SeqCst);
        if self.unknown_once.swap(false, Ordering::SeqCst) {
            let _ = self.inner.submit_verified(payload, options)?;
            return Ok(ClientSubmitResult::UnknownOutcome {
                reason: "post-send lost".into(),
            });
        }
        self.inner.submit_verified(payload, options)
    }
    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        self.inner.query_task_status(info_hash, ownership_tag)
    }
    fn supports_labels(&self) -> bool {
        true
    }
    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn remove_owned(&self, t: &Torrent, d: bool) -> Result<(), DownloaderError> {
        self.inner.remove_owned(t, d)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        self.inner.task_snapshots()
    }
}

pub fn build_policy_pkg(id: &str, hooks_toml: &str, perms: &[&str]) -> Vec<u8> {
    let perms_str = perms
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        "manifest_version = 1\nid = \"{id}\"\nname = \"Policy {id}\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"test\"]\ndescription = \"Hook fixture\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\n\n[[components]]\nid = \"policy\"\nentry = \"policy.wasm\"\nworld = \"{HOOK_WORLD}\"\npermissions = [{perms_str}]\n\n{hooks_toml}\n"
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

pub fn test_site(id: SiteId, url: &str, profile: &str) -> Site {
    Site {
        id,
        name: "TestSite".into(),
        url: url.into(),
        profile_id: profile.into(),
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
    }
}

pub fn test_torrent(site_id: SiteId, id: Option<&str>, title: &str, enclosure: &str) -> Torrent {
    Torrent {
        site_id,
        id: id.map(str::to_string),
        title: title.into(),
        enclosure: enclosure.into(),
        size_bytes: Some(1024),
        seeders: Some(10),
        leechers: Some(1),
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    }
}

pub fn default_plan(torrent: Torrent, purpose: SubmissionPurpose) -> FrozenDownloadPlan {
    let source = downloader::TorrentSource::from_url(&torrent.enclosure).unwrap();
    FrozenDownloadPlan {
        torrent,
        source,
        purpose,
        downloader_id: None,
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
        tags: Vec::new(),
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    }
}

pub fn op_ctx(idempotency_key: &str) -> DownloadOpContext {
    let op_id = PluginOperationId::new();
    DownloadOpContext {
        operation_id: op_id,
        cause_id: format!("cause:{idempotency_key}"),
        subject: "user:test".into(),
        idempotency_key: idempotency_key.into(),
        plugin_id: None,
        generation: None,
        config_revision: None,
        dry_run: false,
    }
}

pub fn make_env(dl: Arc<dyn Downloader>, policy: Option<net_guard::TargetPolicy>) -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let mut state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    if let Some(pol) = policy {
        state = state.with_plugin_http_policy_override(pol);
    }
    let app = api::router(state.clone());
    HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    }
}

pub async fn install_dl_hooks(env: &HttpEnv, id: &str, perms: &[&str], hooks_toml: &str) {
    env.enable_dev_mode().await;
    let caps: Vec<_> = perms
        .iter()
        .map(|c| serde_json::json!({ "capability": c, "scope": "Global", "sensitive": false }))
        .collect();
    let pkg = build_policy_pkg(id, hooks_toml, perms);
    let inst = env.install_pkg(&pkg, None, serde_json::json!(caps)).await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin(id, rev, &format!("en-{id}")).await;
}

pub fn insert_tx_downloader(store: &Store, id: DownloaderId, url: &str, is_default: bool) {
    store
        .insert_downloader(&api::store::DownloaderRow {
            id,
            name: format!("tx-{id}"),
            kind: "transmission".into(),
            url: url.into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default,
            enabled: true,
        })
        .unwrap();
}
