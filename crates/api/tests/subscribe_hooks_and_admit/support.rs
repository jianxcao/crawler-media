use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{Site, SiteId, SubscribeId};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, MemoryDownloader, SubmitOptions,
    SubmitPayload, TaskSnapshot,
};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_api::HOOK_WORLD;
use serde_json::json;

use super::plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, pkg, req_json};

pub const POLICY_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");

pub struct StubTorrentServer {
    pub addr: std::net::SocketAddr,
    pub status_override: Arc<Mutex<u16>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubTorrentServer {
    pub fn start(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let status_override = Arc::new(Mutex::new(200u16));
        let stop_c = Arc::clone(&stop);
        let status_c = Arc::clone(&status_override);
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/default");
                        let code = *status_c.lock().unwrap();
                        if code != 200 {
                            let hdr = format!(
                                "HTTP/1.1 {code} ERR\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.flush();
                            continue;
                        }
                        let resp_body = if path.contains("submit") {
                            minimal_torrent_bytes("The.Matrix.1999.2160p", body.len() as u64)
                        } else {
                            body.clone()
                        };
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            resp_body.len()
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                        let _ = stream.write_all(&resp_body);
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
            status_override,
            stop,
            handle: Some(handle),
        }
    }

    pub fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/{name}", self.addr.port())
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

pub fn minimal_torrent_bytes(name: &str, length: u64) -> Vec<u8> {
    let info = format!(
        "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        name.len()
    );
    format!("d4:info{info}e").into_bytes()
}

pub struct StubSearchFetcher {
    pub payload: String,
}

impl Fetcher for StubSearchFetcher {
    fn fetch(&self, _req: &FetchRequest) -> Result<String, IndexerError> {
        Ok(self.payload.clone())
    }
}

pub struct LockCheckDownloader {
    pub inner: MemoryDownloader,
    pub sub_id_holder: Arc<Mutex<Option<SubscribeId>>>,
    pub state: Arc<Mutex<Option<ApiState>>>,
    pub guard_checked: Arc<AtomicBool>,
}

impl Downloader for LockCheckDownloader {
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if let (Some(st), Some(sub_id)) = (
            self.state.lock().unwrap().as_ref(),
            *self.sub_id_holder.lock().unwrap(),
        ) {
            assert!(
                st.subscribe_guard(sub_id).try_lock().is_some(),
                "subscribe_guard must not be held during submit_verified"
            );
            self.guard_checked.store(true, Ordering::SeqCst);
        }
        self.inner.submit_verified(payload, options)
    }

    fn completed_files(
        &self,
        t: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn remove_owned(&self, t: &domain::Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.remove_owned(t, delete_files)
    }
    fn remember_identity(&self, t: &domain::Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.inner.remember_identity(t, hash)
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
        r#"manifest_version = 1
id = "{id}"
name = "Policy {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "Hook fixture"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = [{perms_str}]

{hooks_toml}
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

pub fn nexus_row(title: &str, href: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><body><table class="torrents"><tr class="torrent"><td><a class="name" title="{title}" href="details.php?id=1">x</a></td><td><a class="download" href="{href}"></a></td><td class="size">4.00 GB</td><td class="seeders">42</td><td class="leechers">7</td><td class="snatched">180</td><td class="upload_time">2024-01-15 08:30</td><td class="category">Movies</td></tr></table></body></html>"#
    )
}

pub async fn seed_site_and_subscribe(env: &HttpEnv, base_url: &str) -> (SiteId, SubscribeId) {
    let site_id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "TestSite".into(),
            url: base_url.into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=abc".into()),
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
    let (st_rs, _, b_rs) = env
        .send(req_json(
            "POST",
            "/api/v1/rule-sets",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "Default",
                "atoms": [{ "kind": "title_match", "value": "Matrix", "priority": 80 }]
            })),
        ))
        .await;
    let rs = assert_ok(st_rs, &b_rs, StatusCode::CREATED);
    let filter_id = rs["id"].as_str().unwrap();
    let (st_sub, _, b_sub) = env
        .send(req_json(
            "POST",
            "/api/v1/subscriptions",
            Some(TOKEN),
            &[],
            Some(json!({
                "media": {
                    "kind": "movie",
                    "title": "The Matrix",
                    "original_title": "The Matrix",
                    "year": 1999
                },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_id
            })),
        ))
        .await;
    let sub = assert_ok(st_sub, &b_sub, StatusCode::CREATED);
    (site_id, sub["id"].as_str().unwrap().parse().unwrap())
}

pub async fn enable_hooks_plugin(env: &HttpEnv, id: &str, hooks: &[&str]) {
    env.enable_dev_mode().await;
    let hooks_toml: String = hooks
        .iter()
        .map(|h| format!("[[components.hooks]]\nid = \"{h}\"\npriority = 10\n"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut perms: Vec<String> = hooks.iter().map(|h| format!("hooks:{h}")).collect();
    for h in hooks {
        if matches!(
            *h,
            "site.checkin.after" | "download.after" | "transfer.completed"
        ) {
            perms.push(format!("events:{h}"));
        }
    }
    let perm_refs: Vec<&str> = perms.iter().map(String::as_str).collect();
    let caps: Vec<_> = perm_refs
        .iter()
        .map(|c| json!({ "capability": c, "scope": "Global", "sensitive": false }))
        .collect();
    let pkg_bytes = build_policy_pkg(id, &hooks_toml, &perm_refs);
    let inst = env.install_pkg(&pkg_bytes, None, json!(caps)).await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin(id, rev, &format!("en-{id}")).await;
}

pub struct SubTestCtx {
    pub env: HttpEnv,
    pub srv: StubTorrentServer,
    pub dl: Arc<LockCheckDownloader>,
    pub guard_checked: Arc<AtomicBool>,
    pub site_id: SiteId,
    pub sub_id: SubscribeId,
}

pub async fn setup_subscribe_env(title: &str, href: &str) -> SubTestCtx {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let torrent_bytes = minimal_torrent_bytes("The.Matrix.1999.1080p", 4096);
    let srv = StubTorrentServer::start(torrent_bytes);
    let sub_holder: Arc<Mutex<Option<SubscribeId>>> = Arc::new(Mutex::new(None));
    let state_holder: Arc<Mutex<Option<ApiState>>> = Arc::new(Mutex::new(None));
    let guard_checked = Arc::new(AtomicBool::new(false));
    let dl = Arc::new(LockCheckDownloader {
        inner: MemoryDownloader::new(dir.path().join("stage")),
        sub_id_holder: Arc::clone(&sub_holder),
        state: Arc::clone(&state_holder),
        guard_checked: Arc::clone(&guard_checked),
    });
    let payload = nexus_row(title, href);
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubSearchFetcher { payload }),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr),
    );
    *state_holder.lock().unwrap() = Some(state.clone());
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state.clone()),
        member_token: String::new(),
    };
    let (site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    *sub_holder.lock().unwrap() = Some(sub_id);
    SubTestCtx {
        env,
        srv,
        dl,
        guard_checked,
        site_id,
        sub_id,
    }
}

pub fn read_full_http_request(s: &mut std::net::TcpStream) -> String {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut expected_total = None;
    while let Ok(n) = s.read(&mut chunk) {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if header_end.is_none()
            && let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n")
        {
            header_end = Some(pos + 4);
            let hdrs = String::from_utf8_lossy(&raw[..pos + 4]);
            let cl = hdrs
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            expected_total = Some(pos + 4 + cl);
        }
        if expected_total.is_some_and(|tot| raw.len() >= tot) {
            break;
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

pub fn spawn_tr_add_stub(hash: &'static str) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let adds = Arc::new(AtomicUsize::new(0));
    let adds_c = Arc::clone(&adds);
    std::thread::spawn(move || {
        while let Ok((mut s, _)) = listener.accept() {
            let req = read_full_http_request(&mut s);
            let body = if req.contains("torrent-add") {
                adds_c.fetch_add(1, Ordering::SeqCst);
                format!(
                    r#"{{"result":"success","arguments":{{"torrent-added":{{"hashString":"{hash}"}}}}}}"#
                )
            } else if req.contains("torrent-get") {
                r#"{"result":"success","arguments":{"torrents":[]}}"#.to_string()
            } else {
                r#"{"result":"success","arguments":{"rpc-version":17,"download-dir":"/downloads"}}"#
                    .to_string()
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (addr, adds)
}
