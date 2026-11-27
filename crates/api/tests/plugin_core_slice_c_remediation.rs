#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::{ApiState, PosterFetch, Store, router};
use axum::http::StatusCode;
use domain::{Confidence, Media, MediaKind, Site, SiteId, SubscribeId, Torrent};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, MemoryDownloader, SubmitOptions,
    SubmitPayload, torrent_info_hash,
};
use indexer::{
    AuthType, FetchMethod, FetchRequest, IndexerError, IndexerProvider, MaintenanceAction,
    MaintenanceCallContext, MaintenanceDescriptor, MaintenanceOutcome, MaintenanceProvider,
    MaintenanceState, ProviderAvailability, ProviderCallContext, ProviderCapability,
    ProviderDescriptor, ProviderSource, RegistryCandidate, RegistrySnapshot, ResolvedDownload,
    SearchQuery,
};
use jobs::Runner as _;
use plugin_api::GuestRole;
use plugin_host::Invocation;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use serde_json::{Value, json};
use support::{
    StubSearchFetcher, StubTorrentServer, enable_hooks_plugin, minimal_torrent_bytes, nexus_row,
};
use tower::ServiceExt;

fn read_http_req(s: &mut std::net::TcpStream) -> String {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut needed = None;
    loop {
        let n = s.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if needed.is_none()
            && let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let hdrs = String::from_utf8_lossy(&raw[..pos]);
            let clen = hdrs
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .unwrap_or(0);
            needed = Some(pos + 4 + clen);
        }
        if needed.is_some_and(|tot| raw.len() >= tot) {
            break;
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

fn spawn_one_shot_torrent(
    bytes: Vec<u8>,
    on_fetch: Option<Box<dyn FnOnce() + Send>>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let cap_c = Arc::clone(&captured);
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            cap_c.lock().unwrap().push(read_http_req(&mut s));
            if let Some(cb) = on_fetch {
                cb();
            }
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = s.write_all(hdr.as_bytes());
            let _ = s.write_all(&bytes);
            let _ = s.flush();
        }
    });
    (addr, captured)
}

fn make_custom_env(
    dir: &tempfile::TempDir,
    payload: &str,
    dl: Arc<dyn Downloader>,
    loopback_addr: Option<SocketAddr>,
) -> HttpEnv {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let snap = RegistrySnapshot::builtin().unwrap();
    let fetcher = Arc::new(StubSearchFetcher {
        payload: payload.into(),
    });
    let mut state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        snap,
        fetcher,
        dl,
    )
    .unwrap();
    if let Some(addr) = loopback_addr {
        let pol = net_guard::TargetPolicy::production().with_test_loopback_socket(addr);
        state = state.with_plugin_http_policy_override(pol);
    }
    HttpEnv {
        _dir: tempfile::tempdir_in("/tmp").unwrap(),
        app: router(state.clone()),
        state,
        member_token: String::new(),
    }
}

fn seed_site(env: &HttpEnv, name: &str, url: &str, profile: &str) -> SiteId {
    let id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: profile.into(),
            cookie: Some("uid=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

fn sample_torrent(site_id: SiteId, id: &str, title: &str, enc: &str) -> Torrent {
    Torrent {
        site_id,
        id: Some(id.into()),
        title: title.into(),
        enclosure: enc.into(),
        size_bytes: Some(4096),
        seeders: Some(20),
        leechers: Some(1),
        snatched: Some(50),
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: Some("Movies".into()),
    }
}

async fn post_json(
    env: &HttpEnv,
    uri: &str,
    hdrs: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json("POST", uri, Some(TOKEN), hdrs, Some(body)))
        .await;
    (st, b)
}

async fn req_authed(env: &HttpEnv, method: &str, uri: &str) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json(method, uri, Some(TOKEN), &[], None))
        .await;
    (st, b)
}

async fn create_sub(env: &HttpEnv, kind: &str, title: &str) -> SubscribeId {
    let cov = if kind == "tv" {
        json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 })
    } else {
        json!({ "kind": "movie" })
    };
    let body = json!({
        "media": { "kind": kind, "title": title, "original_title": title },
        "coverage": cov, "fetch_mode": "search"
    });
    let (st, b) = post_json(env, "/api/v1/subscriptions", &[], body).await;
    assert_ok(st, &b, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn run_claimed_job(env: &HttpEnv, pred: impl Fn(&jobs::Job) -> bool) -> Result<(), String> {
    let job = {
        let jobs = env.state.jobs();
        let q = jobs.acquire().unwrap();
        q.claim(store::unix_now() + 3600, 10)
            .unwrap()
            .into_iter()
            .find(pred)
            .unwrap()
    };
    let st = env.state.clone();
    tokio::task::spawn_blocking(move || api::worker::StateRunner { state: st }.run(&job))
        .await
        .unwrap()
}

fn set_sub_season(store: &Store, sub_id: SubscribeId, season: u32) {
    let mut sub = store.get_subscribe(sub_id).unwrap().unwrap();
    sub.coverage = domain::Coverage::Tv {
        season,
        episode_from: 1,
        episode_to: Some(2),
    };
    store.update_subscribe(&sub).unwrap();
}

// Defect #1: Scheduled search expected_subscribe & post-fetch fencing
#[tokio::test]
async fn scheduled_search_aborts_before_downloader_submit_when_subscribe_changes_mid_fetch() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let t_bytes = minimal_torrent_bytes("The.Matrix.S01E01.1080p.WEB-DL", 4096);
    let mutate_slot = Arc::new(Mutex::new(None));
    let slot_c = Arc::clone(&mutate_slot);
    let (addr, _) = spawn_one_shot_torrent(
        t_bytes,
        Some(Box::new(move || {
            let (store, sub_id): (Arc<Store>, SubscribeId) = slot_c.lock().unwrap().take().unwrap();
            set_sub_season(&store, sub_id, 2);
        })),
    );
    let base = format!("http://127.0.0.1:{}/", addr.port());
    let html = nexus_row(
        "The.Matrix.S01E01.1080p.WEB-DL",
        &format!("{base}m.torrent"),
    );
    let env = make_custom_env(&dir, &html, dl.clone(), Some(addr));
    seed_site(&env, "SchedSite", &base, "demo");
    let sub_id = create_sub(&env, "tv", "The Matrix").await;
    *mutate_slot.lock().unwrap() = Some((env.state.store(), sub_id));

    {
        let jobs = env.state.jobs();
        let q = jobs.acquire().unwrap();
        let payload = json!({ "subscribe_id": sub_id.to_string() }).to_string();
        q.enqueue(jobs::JobIdentity::subscribe_search(sub_id), &payload, 0)
            .unwrap();
    }
    let _ = run_claimed_job(&env, |j| j.scope == jobs::JobScope::Subscribe(sub_id)).await;
    assert!(dl.submitted().is_empty());
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());
}

// Defect #2a: ADR-0012 removal.rs passes library_id (not operation_id)
#[tokio::test]
async fn library_removal_drain_cancels_library_id_not_operation_id() {
    let env = HttpEnv::new().await;
    let lib_root = tempfile::tempdir_in("/tmp").unwrap();
    let root_s = lib_root.path().to_str().unwrap();
    let s = env.state.store();
    let created = s
        .create_library(MediaKind::Movie, "R", &[root_s], "everyone", false, &[])
        .unwrap();

    let (st, b) = req_authed(&env, "DELETE", &format!("/api/v1/libraries/{}", created.id)).await;
    let op_id = assert_ok(st, &b, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(op_id, created.id);

    for _ in 0..50 {
        if s.library_removal(&op_id)
            .unwrap()
            .is_some_and(|r| r.state == "succeeded")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let check_enter = |id: String| {
        let mut inv = Invocation::new("org.example.p", Duration::from_secs(10));
        inv.library_id = Some(id);
        inv.role = GuestRole::Filter;
        env.state
            .plugins()
            .enter_library_invocation_for_test("org.example.p", 1, &inv)
            .is_ok()
    };
    assert!(!check_enter(created.id));
    assert!(check_enter(op_id));
}

// Defect #2b & #2c: claim pinned target race + sidecar rollback
struct PosterCatalogStub;
impl api::catalog::Catalog for PosterCatalogStub {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://image.tmdb.org/t/p/w500/test.jpg".into()))
    }
}

struct HookedPoster(Mutex<Option<Box<dyn FnOnce() + Send>>>);
impl PosterFetch for HookedPoster {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        if let Some(cb) = self.0.lock().unwrap().take() {
            cb();
        }
        Ok(vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
        ])
    }
}

fn walkdir_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walkdir_files(&p));
        } else if p.is_file() {
            out.push(p);
        }
    }
    out
}

#[tokio::test]
async fn claim_cleans_sidecars_on_rollback_and_rejects_mid_flight_root_hijack() {
    let mut env = HttpEnv::new().await;
    let store = env.state.store();
    let def_lib = store.default_library(MediaKind::Movie).unwrap().unwrap();
    let movie_root = def_lib.root_paths[0].clone();
    store.set_scrape_enabled(true).unwrap();

    let poster_hook = Arc::new(HookedPoster(Mutex::new(None)));
    env.state = env
        .state
        .with_catalog(Arc::new(PosterCatalogStub))
        .with_poster_fetch(poster_hook.clone());
    env.app = router(env.state.clone());

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let parked1 = dir.path().join("parked1.mkv");
    std::fs::write(&parked1, b"video-1").unwrap();
    store
        .insert_unidentified(parked1.display().to_string(), Confidence::Low)
        .unwrap();
    let p1_c = parked1.clone();
    *poster_hook.0.lock().unwrap() = Some(Box::new(move || {
        let rep = p1_c.with_extension("mut");
        std::fs::write(&rep, b"mutated-source-bytes-999").unwrap();
        std::fs::rename(&rep, &p1_c).unwrap();
    }));
    let body1 = json!({
        "path": parked1.display().to_string(),
        "action": { "type": "identify_and_transfer", "kind": "movie", "title": "Rollback", "year": 2024, "tmdb_id": "42" }
    });
    let (st_rb, _) = post_json(&env, "/api/v1/unidentified/claim", &[], body1).await;
    assert_eq!(st_rb, StatusCode::CONFLICT);
    assert!(walkdir_files(&movie_root).is_empty());

    enable_hooks_plugin(&env, "org.example.claim-hijack", &["transfer.rename"]).await;
    let parked2 = dir.path().join("parked2.mkv");
    std::fs::write(&parked2, b"video-2").unwrap();
    store
        .insert_unidentified(parked2.display().to_string(), Confidence::Low)
        .unwrap();
    let nested = movie_root.join("renamed");
    *poster_hook.0.lock().unwrap() = Some(Box::new(move || {
        std::fs::create_dir_all(&nested).unwrap();
        let r = nested.to_str().unwrap();
        store
            .create_library(MediaKind::Movie, "Hijack", &[r], "everyone", false, &[])
            .unwrap();
    }));
    let body2 = json!({
        "path": parked2.display().to_string(),
        "action": { "type": "identify_and_transfer", "kind": "movie", "title": "Hijacked", "year": 2024, "tmdb_id": "99" }
    });
    let (st_hj, _) = post_json(&env, "/api/v1/unidentified/claim", &[], body2).await;
    assert_eq!(st_hj, StatusCode::CONFLICT);
    assert!(walkdir_files(&movie_root).is_empty());
}

// Defect #4: Manual .torrent rollback by hash + Idempotency-Key pending replay
struct MutatingManualDownloader {
    inner: MemoryDownloader,
    mutate_sub: Mutex<Option<(Arc<Store>, SubscribeId)>>,
}

impl Downloader for MutatingManualDownloader {
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        let res = self.inner.submit_verified(payload, options)?;
        if let Some((store, sub_id)) = self.mutate_sub.lock().unwrap().take() {
            set_sub_season(&store, sub_id, 3);
        }
        Ok(res)
    }
    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(info_hash, delete_files)
    }
    fn remove_owned(&self, _: &Torrent, _: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("unproven HTTP enclosure".into()))
    }
}

#[tokio::test]
async fn manual_http_torrent_rollback_deletes_by_hash_and_idem_replay_restores_pending() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("Manual.Show.S01E01.1080p.WEB-DL", 4096);
    let expected_hash = torrent_info_hash(&t_bytes).unwrap();
    let srv = StubTorrentServer::start(t_bytes);
    let dl = Arc::new(MutatingManualDownloader {
        inner: MemoryDownloader::new(dir.path().join("stage")),
        mutate_sub: Mutex::new(None),
    });
    let env = make_custom_env(&dir, "", dl.clone(), Some(srv.addr));
    let site_id = seed_site(&env, "ManualSite", &srv.url(""), "demo");
    let sub_id = create_sub(&env, "tv", "Manual Show").await;

    *dl.mutate_sub.lock().unwrap() = Some((env.state.store(), sub_id));
    let dl_url = srv.url("s01e01.torrent");
    let body = json!({
        "download_url": dl_url, "title": "Manual.Show.S01E01.1080p.WEB-DL",
        "site_id": site_id.to_string(), "subscribe_id": sub_id.to_string(),
    });
    let hdr = [("idempotency-key", "manual-rb-idem-1")];
    let (st1, b1) = post_json(&env, "/api/v1/downloaders/submit", &hdr, body.clone()).await;
    assert_err(st1, &b1, StatusCode::CONFLICT, "subscription.changed");
    assert_eq!(dl.inner.deleted_tasks(), vec![(expected_hash, false)]);

    set_sub_season(env.state.store().as_ref(), sub_id, 1);

    let (st2, b2) = post_json(&env, "/api/v1/downloaders/submit", &hdr, body.clone()).await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 2);
    assert_eq!(env.state.store().load_pending(sub_id).unwrap().len(), 1);

    env.state.store().delete_pending(sub_id, &dl_url).unwrap();
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());

    let (st3, b3) = post_json(&env, "/api/v1/downloaders/submit", &hdr, body).await;
    assert_ok(st3, &b3, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 2);
    assert_eq!(env.state.store().load_pending(sub_id).unwrap().len(), 1);
}

// Defect #5: Single download-token resolution & full ResolvedDownload::Request
struct CountingPostIdxProvider {
    desc: ProviderDescriptor,
    dl_url: String,
    resolve_calls: Arc<AtomicUsize>,
}

impl IndexerProvider for CountingPostIdxProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.desc
    }
    fn search(
        &self,
        _: &ProviderCallContext<'_>,
        site: &Site,
        _: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let t1 = "The.Matrix.1999.1080p.BluRay.x264";
        let t2 = "The.Matrix.RejectDownload.1080p";
        Ok(vec![
            sample_torrent(site.id, "1", t1, "dlref-one-shot"),
            sample_torrent(site.id, "2", t2, "dlref-reject"),
        ])
    }
    fn rss(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }
    fn resolve_download(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        self.resolve_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(ResolvedDownload::Request(FetchRequest {
            key: "dl".into(),
            url: self.dl_url.clone(),
            method: FetchMethod::PostJson,
            body: Some("{\"token\":\"single-use-123\"}".into()),
            cookie: Some("dl_auth=secret_tok".into()),
            api_key: None,
            proxy: None,
            cdp_url: None,
        })))
    }
}

fn wasm_src(pid: &str, cid: &str) -> ProviderSource {
    ProviderSource::PluginWasm {
        plugin_id: pid.into(),
        component_id: cid.into(),
        version: "1.0.0".into(),
        plugin_generation: 1,
    }
}

#[tokio::test]
async fn search_admit_resolves_download_token_once_after_hook_and_preserves_post_request() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("The.Matrix.1999.1080p.BluRay.x264", 4096);
    let (addr, captured) = spawn_one_shot_torrent(t_bytes, None);
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_custom_env(&dir, "", dl, Some(addr));
    enable_hooks_plugin(&env, "org.example.admit-gate", &["download.before"]).await;

    let resolve_calls = Arc::new(AtomicUsize::new(0));
    let pid = "plugin:org.example.oneshot/idx";
    let base = format!("http://127.0.0.1:{}/", addr.port());
    let caps = vec![
        ProviderCapability::Search,
        ProviderCapability::ResolveDownload,
    ];
    let prov = CountingPostIdxProvider {
        desc: ProviderDescriptor {
            id: pid.into(),
            name: "OneShotIdx".into(),
            kind: "wasm".into(),
            source: wasm_src("org.example.oneshot", "idx"),
            auth_types: vec![AuthType::Cookie],
            capabilities: caps,
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog: false,
        },
        dl_url: format!("{base}download-post"),
        resolve_calls: Arc::clone(&resolve_calls),
    };
    let cand = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(prov))
        .unwrap();
    env.state.indexer().publish(cand).unwrap();
    seed_site(&env, "OneShotSite", &base, pid);
    let sub_id = create_sub(&env, "movie", "The Matrix").await.to_string();

    let rej = json!({ "subscribe_id": sub_id, "enclosure": "dlref-reject" });
    let (st_rej, b_rej) = post_json(&env, "/api/v1/search/admit", &[], rej).await;
    assert_err(st_rej, &b_rej, StatusCode::FORBIDDEN, "plugin.rejected");
    assert_eq!(resolve_calls.load(Ordering::SeqCst), 0);

    let ok = json!({ "subscribe_id": sub_id, "enclosure": "dlref-one-shot" });
    let (st_ok, b_ok) = post_json(&env, "/api/v1/search/admit", &[], ok).await;
    assert_ok(st_ok, &b_ok, StatusCode::OK);
    assert_eq!(resolve_calls.load(Ordering::SeqCst), 1);
    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].starts_with("POST /download-post "), "{}", reqs[0]);
    assert!(reqs[0].contains("{\"token\":\"single-use-123\"}"));
    assert!(reqs[0].contains("dl_auth=secret_tok"));
}

// Defect #6: WASM site maintenance "login" action consistency
struct WasmMaintStub {
    desc: MaintenanceDescriptor,
    calls: Arc<AtomicUsize>,
}

impl MaintenanceProvider for WasmMaintStub {
    fn descriptor(&self) -> &MaintenanceDescriptor {
        &self.desc
    }
    fn run(
        &self,
        _: &MaintenanceCallContext<'_>,
        _: &Site,
        action: MaintenanceAction,
        _: &str,
    ) -> Result<MaintenanceOutcome, IndexerError> {
        assert_eq!(action, MaintenanceAction::VerifySession);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(MaintenanceOutcome {
            state: MaintenanceState::Completed,
            evidence: "wasm-verify-ok".into(),
            session_capture: None,
        })
    }
}

fn mk_wasm_maint(pid: &str, act: MaintenanceAction, calls: Arc<AtomicUsize>) -> Arc<WasmMaintStub> {
    Arc::new(WasmMaintStub {
        desc: MaintenanceDescriptor {
            id: format!("plugin:{pid}/maint"),
            source: wasm_src(pid, "maint"),
            actions: vec![act],
            availability: ProviderAvailability::Enabled,
        },
        calls,
    })
}

async fn post_site_login_action(env: &HttpEnv, site: SiteId, key: &str) -> (StatusCode, Value) {
    let obs = env.state.store().observe_site(site).unwrap().unwrap();
    let ifm = format!("\"{}\"", obs.revision());
    let hdr = [("if-match", ifm.as_str()), ("idempotency-key", key)];
    let uri = format!("/api/v1/sites/{site}/plugin-actions");
    post_json(env, &uri, &hdr, json!({ "action": "login" })).await
}

#[tokio::test]
async fn wasm_site_login_routes_through_verify_session_and_rejects_unsupported() {
    let env = HttpEnv::new().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let p_ok = mk_wasm_maint(
        "org.example.wl",
        MaintenanceAction::VerifySession,
        Arc::clone(&calls),
    );
    let p_bad = mk_wasm_maint(
        "org.example.co",
        MaintenanceAction::CheckIn,
        Arc::new(AtomicUsize::new(0)),
    );
    let cand = RegistryCandidate::from_builtin()
        .unwrap()
        .add_maintenance_provider(p_ok)
        .unwrap()
        .add_maintenance_provider(p_bad)
        .unwrap();
    env.state.indexer().publish(cand).unwrap();

    let u = "https://wasm.example/";
    let ok_fg = seed_site(&env, "Fg", u, "plugin:org.example.wl/idx");
    let ok_pa = seed_site(&env, "Pa", u, "plugin:org.example.wl/idx");
    let bad = seed_site(&env, "Bad", u, "plugin:org.example.co/idx");
    let login_u = |s: SiteId| format!("/api/v1/sites/{s}/login");

    let (st_fg, b_fg) = post_json(&env, &login_u(ok_fg), &[], json!({})).await;
    assert_ok(st_fg, &b_fg, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let (st_pa, b_pa) = post_site_login_action(&env, ok_pa, "wasm-pa-1").await;
    let op_id = assert_ok(st_pa, &b_pa, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    run_claimed_job(&env, |j| j.kind == jobs::JobKind::PluginCommand)
        .await
        .unwrap();
    let (st_op, b_op) =
        req_authed(&env, "GET", &format!("/api/v1/plugin-operations/{op_id}")).await;
    assert_eq!(
        assert_ok(st_op, &b_op, StatusCode::OK)["state"],
        "succeeded"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let (st_b1, b_b1) = post_json(&env, &login_u(bad), &[], json!({})).await;
    let code = "site.maintenance_unsupported";
    assert_err(st_b1, &b_b1, StatusCode::BAD_REQUEST, code);
    let (st_b2, b_b2) = post_site_login_action(&env, bad, "wasm-bad-1").await;
    assert_err(st_b2, &b_b2, StatusCode::BAD_REQUEST, code);
}

// Defect #7: SSE search done.items post-hook reconciliation
#[tokio::test]
async fn search_stream_done_carries_post_hook_reconciled_items() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let html = r#"<!DOCTYPE html><html><body><table class="torrents">
        <tr class="torrent"><td><a class="name" title="A" href="details.php?id=1">x</a></td><td><a class="download" href="download.php?id=1"></a></td><td class="size">1.00 GB</td><td class="seeders">10</td><td class="leechers">1</td><td class="snatched">5</td><td class="upload_time">2024-01-15 08:30</td><td class="category">Movies</td></tr>
        <tr class="torrent"><td><a class="name" title="Matrix.2024.1080p" href="details.php?id=2">x</a></td><td><a class="download" href="download.php?id=2"></a></td><td class="size">2.00 GB</td><td class="seeders">20</td><td class="leechers">1</td><td class="snatched">5</td><td class="upload_time">2024-01-15 08:30</td><td class="category">Movies</td></tr>
    </table></body></html>"#;
    let env = make_custom_env(&dir, html, dl, None);
    env.insert_site("StreamSite");
    let hooks = ["search.site.after", "search.after", "filter.after"];
    enable_hooks_plugin(&env, "org.example.stream-hooks", &hooks).await;

    let uri = "/api/v1/search/torrents/stream?keyword=Matrix";
    let req = req_json("GET", uri, Some(TOKEN), &[], None);
    let resp = env.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    let done_data: Value = raw
        .split("\n\n")
        .find_map(|b| {
            b.contains("event: done")
                .then(|| b.lines().find_map(|l| l.strip_prefix("data: ")))
                .flatten()
        })
        .and_then(|d| serde_json::from_str(d).ok())
        .expect("done SSE event");
    let done_items = done_data["items"].as_array().expect("done.items array");
    assert_eq!(done_items.len(), 1);
    assert_eq!(done_items[0]["title"], "Matrix.2024.1080p");
}

// Defect #8: release-forecast timestamp formats via shared parser
#[tokio::test]
async fn release_forecast_parses_rfc2822_sql_utc_and_epoch_timestamps() {
    let env = HttpEnv::new().await;
    let site_id = env.insert_site("ForecastSite");
    let sub_id = create_sub(&env, "tv", "Forecast Show").await;
    let recent_s = store::unix_now() - 86_400;
    let dt = time::OffsetDateTime::from_unix_timestamp(recent_s).unwrap();
    let rfc2822 = dt
        .format(&time::format_description::well_known::Rfc2822)
        .unwrap();
    let fmt = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day] [hour]:[minute]:[second]",
    )
    .unwrap();
    let sql_utc = dt.format(&fmt).unwrap();
    let samples = [
        rfc2822,
        sql_utc,
        recent_s.to_string(),
        (recent_s * 1_000).to_string(),
    ];
    for (i, ts) in samples.into_iter().enumerate() {
        let (ep, enc) = (
            format!("Forecast.Show.S01E0{}.1080p", i + 1),
            format!("https://pt.example/f/{i}.torrent"),
        );
        let mut t = sample_torrent(site_id, "0", &ep, &enc);
        t.upload_time = Some(ts);
        let pending = api::store::PendingDownload {
            torrent: t,
            release_override: None,
            downloader_id: None,
            submitted_at: Some(recent_s),
        };
        env.state
            .store()
            .record_pending_submission(sub_id, 80, &pending)
            .unwrap();
    }
    let (st_fc, b_fc) = req_authed(
        &env,
        "GET",
        &format!("/api/v1/subscriptions/{sub_id}/release-forecast"),
    )
    .await;
    assert_eq!(assert_ok(st_fc, &b_fc, StatusCode::OK)["samples"], 4);
}
