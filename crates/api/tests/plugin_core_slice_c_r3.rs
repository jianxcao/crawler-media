#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::catalog::Catalog;
use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadOutcome, FrozenDownloadPlan,
    PosterFetch, Store, SubmissionPurpose, TorrentSource, router,
};
use axum::http::StatusCode;
use domain::{
    Confidence, DownloaderId, LibraryId, Media, MediaKind, PluginOperationId, SiteId, SubscribeId,
    Torrent,
};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, MemoryDownloader, SubmitOptions,
    SubmitPayload, torrent_info_hash,
};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use jobs::Runner as _;
use media::CatalogHit;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use serde_json::{Value, json};
use store::DownloadOwnershipState;
use support::{StubSearchFetcher, StubTorrentServer, enable_hooks_plugin, minimal_torrent_bytes};
use uuid::Uuid;

fn make_env<F: Fetcher + 'static>(
    dir: &tempfile::TempDir,
    fetcher: Arc<F>,
    dl: Arc<dyn Downloader>,
    addrs: &[SocketAddr],
) -> HttpEnv {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let snap = RegistrySnapshot::builtin().unwrap();
    let pass = Some(ADMIN_PASS.into());
    let mut state =
        ApiState::new_arc_with_admin_password(store, TOKEN.into(), pass, snap, fetcher, dl)
            .unwrap();
    if !addrs.is_empty() {
        let pol = addrs
            .iter()
            .fold(net_guard::TargetPolicy::production(), |p, a| {
                p.with_test_loopback_socket(*a)
            });
        state = state.with_plugin_http_policy_override(pol);
    }
    HttpEnv {
        _dir: tempfile::tempdir_in("/tmp").unwrap(),
        app: router(state.clone()),
        state,
        member_token: String::new(),
    }
}

fn make_custom_env(
    dir: &tempfile::TempDir,
    dl: Arc<dyn Downloader>,
    addrs: &[SocketAddr],
) -> HttpEnv {
    let f = Arc::new(StubSearchFetcher { payload: "".into() });
    make_env(dir, f, dl, addrs)
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

async fn seed_site_http(env: &HttpEnv, name: &str, url: &str, profile: &str) -> SiteId {
    let body = json!({
        "name": name, "url": url, "profile_id": profile,
        "cookie": "uid=1; pass=init", "maintenance_via": "http", "timezone": "UTC", "enabled": true
    });
    let (st, b) = post_json(env, "/api/v1/sites", &[], body).await;
    assert_ok(st, &b, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn create_tv_sub(env: &HttpEnv, title: &str, ep_to: u32) -> SubscribeId {
    let cov = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": ep_to });
    let media = json!({ "kind": "tv", "title": title, "original_title": title });
    let body =
        json!({ "media": media, "coverage": cov, "fetch_mode": "search", "follow_future": false });
    let (st, b) = post_json(env, "/api/v1/subscriptions", &[], body).await;
    assert_ok(st, &b, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
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

async fn run_job(env: &HttpEnv, pred: impl Fn(&jobs::Job) -> bool) -> Result<(), String> {
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

async fn enqueue_and_run_search(env: &HttpEnv, sub_id: SubscribeId) -> Result<(), String> {
    {
        let jobs = env.state.jobs();
        let q = jobs.acquire().unwrap();
        let payload = json!({ "subscribe_id": sub_id.to_string() }).to_string();
        q.enqueue(jobs::JobIdentity::subscribe_search(sub_id), &payload, 0)
            .unwrap();
    }
    run_job(env, |j| j.scope == jobs::JobScope::Subscribe(sub_id)).await
}

fn build_plan_and_ctx(
    (site_id, url, title): (SiteId, &str, &str),
    purpose: SubmissionPurpose,
    subscribe_id: Option<SubscribeId>,
    plugin_id: Option<&str>,
    idem_key: &str,
) -> (FrozenDownloadPlan, DownloadOpContext) {
    let torrent = Torrent {
        site_id,
        id: Some("1".into()),
        title: title.into(),
        enclosure: url.into(),
        size_bytes: Some(4096),
        seeders: Some(10),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
    };
    let plan = FrozenDownloadPlan {
        torrent,
        source: TorrentSource::from_url(url).unwrap(),
        purpose,
        downloader_id: None,
        frozen_endpoint: None,
        subscribe_id,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: Some(80),
        release_override: None,
        force: false,
        save_path: None,
        category: None,
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    };
    let op_id = PluginOperationId::new();
    let ctx = DownloadOpContext {
        operation_id: op_id,
        cause_id: format!("r3:{op_id}"),
        subject: "user:r3".into(),
        idempotency_key: idem_key.into(),
        plugin_id: plugin_id.map(str::to_string),
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, ctx)
}

fn manual_submit_json(url: &str, title: &str, site_id: SiteId, sub_id: SubscribeId) -> Value {
    json!({
        "download_url": url, "title": title,
        "site_id": site_id.to_string(), "subscribe_id": sub_id.to_string(),
    })
}

struct CallbackSubmitDownloader {
    inner: MemoryDownloader,
    identities: Mutex<HashMap<String, String>>,
    on_submit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    on_delete: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl CallbackSubmitDownloader {
    fn new(stage: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryDownloader::new(stage),
            identities: Mutex::new(HashMap::new()),
            on_submit: Mutex::new(None),
            on_delete: Mutex::new(None),
        })
    }
}

impl Downloader for CallbackSubmitDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.identities
            .lock()
            .unwrap()
            .insert(torrent.enclosure.clone(), hash.to_string());
        self.inner.remember_identity(torrent, hash)
    }
    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        if let Some(h) = self.identities.lock().unwrap().get(&torrent.enclosure) {
            return Ok(Some(h.clone()));
        }
        self.inner.owned_identity(torrent)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        let res = self.inner.submit_verified(payload, options)?;
        if let Some(cb) = self.on_submit.lock().unwrap().take() {
            cb();
        }
        Ok(res)
    }
    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        if let Some(cb) = self.on_delete.lock().unwrap().take() {
            cb();
        }
        self.inner.delete_task(info_hash, delete_files)
    }
}

// --- Defect #1: remove_one_task transitions download_ownership to Deleted ---

#[tokio::test]
async fn delete_subscription_marks_download_ownership_deleted_for_owned_tasks() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t1 = minimal_torrent_bytes("Del.Show.S01E01.1080p.WEB-DL", 4096);
    let t2 = minimal_torrent_bytes("Del.Plugin.S01E01.1080p.WEB-DL", 4096);
    let (h1, h2) = (
        torrent_info_hash(&t1).unwrap(),
        torrent_info_hash(&t2).unwrap(),
    );
    let (srv1, srv2) = (StubTorrentServer::start(t1), StubTorrentServer::start(t2));
    let dl = CallbackSubmitDownloader::new(dir.path().join("stage"));
    let env = make_custom_env(&dir, dl, &[srv1.addr, srv2.addr]);
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());

    let site1 = seed_site_http(&env, "DelSite1", &srv1.url(""), "demo").await;
    let sub1 = create_tv_sub(&env, "Del Show", 2).await;
    let b1 = manual_submit_json(
        &srv1.url("e01.torrent"),
        "Del.Show.S01E01.1080p.WEB-DL",
        site1,
        sub1,
    );
    let (st_sub, b_sub) = post_json(&env, "/api/v1/downloaders/submit", &[], b1).await;
    assert_ok(st_sub, &b_sub, StatusCode::OK);
    let del1 = format!("/api/v1/subscriptions/{sub1}?delete_torrents=true");
    let (st_d1, b_d1) = req_authed(&env, "DELETE", &del1).await;
    assert_ok(st_d1, &b_d1, StatusCode::OK);
    let r1 = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &h1)
        .unwrap();
    assert_eq!(r1[0].ownership_state, DownloadOwnershipState::Deleted);

    let site2 = seed_site_http(&env, "DelSite2", &srv2.url(""), "demo").await;
    let sub2 = create_tv_sub(&env, "Del Plugin", 2).await;
    let u2 = srv2.url("p.torrent");
    let (plan2, ctx2) = build_plan_and_ctx(
        (site2, &u2, "Del.Plugin.S01E01.1080p.WEB-DL"),
        SubmissionPurpose::Plugin,
        Some(sub2),
        Some("builtin.boost"),
        "idem-del-plugin",
    );
    ApiDownloadService::new(env.state.clone())
        .submit_sync(plan2.clone(), ctx2)
        .unwrap();
    let p_row = api::store::PendingDownload {
        torrent: plan2.torrent,
        release_override: None,
        downloader_id: None,
        submitted_at: Some(store::unix_now()),
    };
    env.state
        .store()
        .record_pending_submission(sub2, 80, &p_row)
        .unwrap();
    let del2 = format!("/api/v1/subscriptions/{sub2}?delete_torrents=true");
    let (st_d2, b_d2) = req_authed(&env, "DELETE", &del2).await;
    assert_ok(st_d2, &b_d2, StatusCode::OK);
    let r2 = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &h2)
        .unwrap();
    assert_eq!(r2[0].ownership_state, DownloadOwnershipState::Deleted);
}

// --- Defect #2: Boost deduplicating against existing_owned_tag avoids FK error ---

#[tokio::test]
async fn boost_submission_deduping_existing_owned_tag_succeeds_without_fk_error() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("Boost.Dedup.1999.1080p", 4096));
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_custom_env(&dir, dl, &[srv.addr]);
    let site_id = seed_site_http(&env, "BoostSite", &srv.url(""), "demo").await;
    let url = srv.url("boost-dedup.torrent");

    let (m_plan, m_ctx) = build_plan_and_ctx(
        (site_id, &url, "Boost.Dedup.1999.1080p"),
        SubmissionPurpose::Manual,
        None,
        None,
        "idem-first-manual",
    );
    let first_tag = format!("cm-op:{}", m_ctx.operation_id);
    let svc = ApiDownloadService::new(env.state.clone());
    svc.submit_sync(m_plan, m_ctx).unwrap();

    let (b_plan, b_ctx) = build_plan_and_ctx(
        (site_id, &url, "Boost.Dedup.1999.1080p"),
        SubmissionPurpose::Boost,
        None,
        Some("builtin.boost"),
        "idem-second-boost",
    );
    let boost_out = svc.submit_sync(b_plan, b_ctx).unwrap();
    assert!(matches!(
        boost_out,
        DownloadOutcome::Submitted {
            already_owned: true,
            ..
        }
    ));
    let facts = env.state.store().list_active_boost_task_facts().unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].ownership_tag, first_tag);
}

// --- Defect #3: record_gate_reject and record_gate_defer skip rolled-back #rb: slots ---

#[tokio::test]
async fn rolled_back_idempotency_key_skips_rb_slot_on_gate_reject_and_gate_defer() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("The.Matrix.S01E01.1080p", 4096));
    let dl = CallbackSubmitDownloader::new(dir.path().join("stage"));
    let env = make_custom_env(&dir, dl.clone(), &[srv.addr]);
    let site_id = seed_site_http(&env, "RbGateSite", &srv.url(""), "demo").await;
    let sub_id = create_tv_sub(&env, "The Matrix", 2).await;

    let st1 = env.state.store();
    *dl.on_submit.lock().unwrap() = Some(Box::new(move || set_sub_season(&st1, sub_id, 2)));
    let rej_body = manual_submit_json(
        &srv.url("rb-rej.torrent"),
        "The.Matrix.RejectDownload.S01E01.1080p",
        site_id,
        sub_id,
    );
    let rej_hdr = [("idempotency-key", "rb-gate-reject-1")];
    let (s1, b1) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &rej_hdr,
        rej_body.clone(),
    )
    .await;
    assert_err(s1, &b1, StatusCode::CONFLICT, "subscription.changed");
    set_sub_season(env.state.store().as_ref(), sub_id, 1);

    let st2 = env.state.store();
    *dl.on_submit.lock().unwrap() = Some(Box::new(move || set_sub_season(&st2, sub_id, 2)));
    let def_body = manual_submit_json(
        &srv.url("rb-def.torrent"),
        "The.Matrix.DeferDownload.S01E02.1080p",
        site_id,
        sub_id,
    );
    let def_hdr = [("idempotency-key", "rb-gate-defer-1")];
    let (s2, b2) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &def_hdr,
        def_body.clone(),
    )
    .await;
    assert_err(s2, &b2, StatusCode::CONFLICT, "subscription.changed");
    set_sub_season(env.state.store().as_ref(), sub_id, 1);

    enable_hooks_plugin(&env, "org.example.rb-gate", &["download.before"]).await;

    let (sr, br) = post_json(&env, "/api/v1/downloaders/submit", &rej_hdr, rej_body).await;
    assert_err(sr, &br, StatusCode::BAD_REQUEST, "downloader.add_failed");

    let (sd, bd) = post_json(&env, "/api/v1/downloaders/submit", &def_hdr, def_body).await;
    assert_err(sd, &bd, StatusCode::CONFLICT, "plugin.deferred");
}

// --- Defect #4: /downloaders/submit .with_fence, library_id drift, and post-submit ReadBusy ---

#[tokio::test]
async fn manual_submit_fences_mid_fetch_and_post_submit_library_id_drift() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = CallbackSubmitDownloader::new(dir.path().join("stage"));
    let t_bytes = minimal_torrent_bytes("Man.Fence.S01E01.1080p.WEB-DL", 4096);
    let mutate_mf = Arc::new(Mutex::new(None::<Box<dyn FnOnce() + Send>>));
    let mf_c = Arc::clone(&mutate_mf);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr1 = listener.local_addr().unwrap();
    let tb_c = t_bytes.clone();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.read(&mut [0u8; 2048]);
            if let Some(cb) = mf_c.lock().unwrap().take() {
                cb();
            }
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                tb_c.len()
            );
            let _ = s.write_all(hdr.as_bytes());
            let _ = s.write_all(&tb_c);
        }
    });
    let srv2 = StubTorrentServer::start(t_bytes);
    let env = make_custom_env(&dir, dl.clone(), &[addr1, srv2.addr]);

    let lib2_dir = tempfile::tempdir_in("/tmp").unwrap();
    let p2 = lib2_dir.path().to_str().unwrap();
    let lib2_id: LibraryId = env
        .state
        .store()
        .create_library(MediaKind::Tv, "SecondTv", &[p2], "everyone", false, &[])
        .unwrap()
        .id
        .parse()
        .unwrap();

    let base1 = format!("http://127.0.0.1:{}/", addr1.port());
    let site1 = seed_site_http(&env, "FenceSite1", &base1, "demo").await;
    let sub_a = create_tv_sub(&env, "Man Fence", 2).await;
    let st_mf = env.state.store();
    *mutate_mf.lock().unwrap() = Some(Box::new(move || {
        let mut s = st_mf.get_subscribe(sub_a).unwrap().unwrap();
        s.library_id = Some(lib2_id);
        st_mf.update_subscribe(&s).unwrap();
    }));
    let b_a_in = manual_submit_json(
        &format!("{base1}mf.torrent"),
        "Man.Fence.S01E01.1080p.WEB-DL",
        site1,
        sub_a,
    );
    let (st_a, b_a) = post_json(&env, "/api/v1/downloaders/submit", &[], b_a_in).await;
    assert_err(st_a, &b_a, StatusCode::CONFLICT, "subscription.changed");
    assert!(dl.inner.submitted().is_empty());

    let site2 = seed_site_http(&env, "FenceSite2", &srv2.url(""), "demo").await;
    let sub_b = create_tv_sub(&env, "Man Fence", 2).await;
    let st_ps = env.state.store();
    *dl.on_submit.lock().unwrap() = Some(Box::new(move || {
        let mut s = st_ps.get_subscribe(sub_b).unwrap().unwrap();
        s.library_id = Some(lib2_id);
        st_ps.update_subscribe(&s).unwrap();
    }));
    let b_b_in = manual_submit_json(
        &srv2.url("ps.torrent"),
        "Man.Fence.S01E01.1080p.WEB-DL",
        site2,
        sub_b,
    );
    let (st_b, b_b) = post_json(&env, "/api/v1/downloaders/submit", &[], b_b_in).await;
    assert_err(st_b, &b_b, StatusCode::CONFLICT, "subscription.changed");
    assert_eq!(dl.inner.deleted_tasks().len(), 1);
    assert!(env.state.store().load_pending(sub_b).unwrap().is_empty());
}

#[tokio::test]
async fn manual_submit_post_submit_read_busy_rolls_back_and_returns_503() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = CallbackSubmitDownloader::new(dir.path().join("stage"));
    let srv = StubTorrentServer::start(minimal_torrent_bytes("Man.Busy.S01E01.1080p", 4096));
    let env = make_custom_env(&dir, dl.clone(), &[srv.addr]);
    let site_id = seed_site_http(&env, "BusyPostSite", &srv.url(""), "demo").await;
    let sub_id = create_tv_sub(&env, "Man Busy", 2).await;

    let held_leases = Arc::new(Mutex::new(Vec::new()));
    let (hl_sub, hl_del) = (Arc::clone(&held_leases), Arc::clone(&held_leases));
    let pool = env.state.store().reader_pool(store::Db::Subscribe);
    *dl.on_submit.lock().unwrap() = Some(Box::new(move || {
        let mut guards = hl_sub.lock().unwrap();
        while let Ok(lease) = pool.checkout(Duration::ZERO) {
            guards.push(lease);
        }
    }));
    *dl.on_delete.lock().unwrap() = Some(Box::new(move || hl_del.lock().unwrap().clear()));

    let body = manual_submit_json(
        &srv.url("busy.torrent"),
        "Man.Busy.S01E01.1080p",
        site_id,
        sub_id,
    );
    let (st, _) = post_json(&env, "/api/v1/downloaders/submit", &[], body).await;
    held_leases.lock().unwrap().clear();
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(dl.inner.deleted_tasks().len(), 1);
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());
}

// --- Defect #5: ADR-0012 /unidentified/claim library cancel & orphan Media undo ---

struct ClaimPosterCatalog;
impl Catalog for ClaimPosterCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://fixture.invalid/poster.jpg".into()))
    }
}

struct HookedPosterFetch(Mutex<Option<Box<dyn FnOnce() + Send>>>);
impl PosterFetch for HookedPosterFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        if let Some(cb) = self.0.lock().unwrap().take() {
            cb();
        }
        Ok(b"poster-bytes".to_vec())
    }
}

fn claim_movie_json(path: &std::path::Path, title: &str, year: u16, tmdb_id: &str) -> Value {
    json!({
        "path": path,
        "action": { "type": "identify_and_transfer", "title": title, "kind": "movie", "year": year, "tmdb_id": tmdb_id }
    })
}

#[tokio::test]
async fn unidentified_claim_respects_library_cancel_and_undoes_orphan_media_on_commit_failure() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let mut env = make_custom_env(&dir, dl, &[]);
    let poster_hook = Arc::new(HookedPosterFetch(Mutex::new(None)));
    env.state = env
        .state
        .with_catalog(Arc::new(ClaimPosterCatalog))
        .with_poster_fetch(poster_hook.clone());
    env.app = router(env.state.clone());
    env.state.store().set_scrape_enabled(true).unwrap();

    let movie_lib = env
        .state
        .store()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let src1 = dir.path().join("claim_cancel.mkv");
    std::fs::write(&src1, b"video-bytes-1").unwrap();
    env.state
        .store()
        .insert_unidentified(src1.display().to_string(), Confidence::Low)
        .unwrap();
    let st_cancel = env.state.clone();
    *poster_hook.0.lock().unwrap() = Some(Box::new(move || {
        st_cancel.library_work().cancel(&movie_lib)
    }));

    let c1 = claim_movie_json(&src1, "Cancelled Claim Movie", 2024, "501");
    let (st1, b1) = post_json(&env, "/api/v1/unidentified/claim", &[], c1).await;
    assert_err(st1, &b1, StatusCode::CONFLICT, "library.unavailable");
    assert!(env.state.store().list_media().unwrap().is_empty());

    let lib2_dir = tempfile::tempdir_in("/tmp").unwrap();
    let p2 = lib2_dir.path().to_str().unwrap();
    let store = env.state.store();
    store
        .create_library(MediaKind::Movie, "FM", &[p2], "everyone", true, &[])
        .unwrap();
    let src2 = dir.path().join("claim_commit_fail.mkv");
    std::fs::write(&src2, b"video-bytes-2").unwrap();
    let src2_str = src2.display().to_string();
    store
        .insert_unidentified(src2_str.clone(), Confidence::Low)
        .unwrap();
    let lib_db_path = dir.path().join("data/library.db");
    *poster_hook.0.lock().unwrap() = Some(Box::new(move || {
        let conn = rusqlite::Connection::open(lib_db_path).unwrap();
        conn.execute("DELETE FROM unidentified WHERE path = ?1", [&src2_str])
            .unwrap();
    }));

    let c2 = claim_movie_json(&src2, "Orphan Candidate Movie", 2025, "502");
    let (st2, _) = post_json(&env, "/api/v1/unidentified/claim", &[], c2).await;
    assert_ne!(st2, StatusCode::OK);
    assert!(store.list_media().unwrap().is_empty());
}

#[tokio::test]
async fn async_site_plugin_actions_reject_when_site_revision_or_credentials_change() {
    let env = HttpEnv::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = Arc::clone(&hits);
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.read(&mut [0u8; 1024]);
            hits_c.fetch_add(1, Ordering::SeqCst);
        }
    });
    let u = format!("http://{addr}/");
    let site_id = seed_site_http(&env, "MaintRace", &u, "pterclub").await;
    let obs = env.state.store().observe_site(site_id).unwrap().unwrap();
    let ifm = format!("\"{}\"", obs.revision());
    let hdr = [("if-match", ifm.as_str()), ("idempotency-key", "pa-r3-1")];
    let pa_uri = format!("/api/v1/sites/{site_id}/plugin-actions");
    let (st_pa, b_pa) = post_json(&env, &pa_uri, &hdr, json!({ "action": "checkin" })).await;
    let op_id = assert_ok(st_pa, &b_pa, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let s_uri = format!("/api/v1/sites/{site_id}");
    let p_body = Some(json!({ "cookie": "uid=2; pass=rotated" }));
    let (st_p, _, b_p) = env
        .send(req_json("PATCH", &s_uri, Some(TOKEN), &[], p_body))
        .await;
    assert_ok(st_p, &b_p, StatusCode::OK);

    run_job(&env, |j| j.kind == jobs::JobKind::PluginCommand)
        .await
        .unwrap();
    let (st_op, b_op) =
        req_authed(&env, "GET", &format!("/api/v1/plugin-operations/{op_id}")).await;
    assert_eq!(assert_ok(st_op, &b_op, StatusCode::OK)["state"], "rejected");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

// --- Defect #12: worker/search.rs search.before rejected/empty-sites & page-2 run_search_before ---

struct PagingSearchFetcher;
impl Fetcher for PagingSearchFetcher {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        let page = req
            .url
            .split("page=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let mut html = String::from("<a href='logout.php'></a><table class='torrents'>");
        for i in 0..30 {
            html.push_str(&format!(
                "<tr><td class='rowfollow'><a href='details.php' title='Other Show S02E{i:02}'>t</a><a href='download.php?id={page}{i}'>dl</a></td><td class='rowfollow'>1GB</td><td class='rowfollow'>1</td></tr>"
            ));
        }
        html.push_str("</table>");
        Ok(html)
    }
}

#[tokio::test]
async fn subscribe_search_succeeds_when_search_before_rejects_all_keywords_and_runs_on_page_2() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, Arc::new(PagingSearchFetcher), dl, &[]);
    env.state
        .store()
        .put_setting(api::settings_keys::SUBSCRIBE_AIR_DATE_GATE, "false")
        .unwrap();
    seed_site_http(&env, "SbSite", "https://pt.example/", "pterclub").await;
    let pid = "org.example.sb-r3";
    enable_hooks_plugin(&env, pid, &["search.before"]).await;

    let m_body = json!({
        "media": { "kind": "movie", "title": "reject-me" },
        "coverage": { "kind": "movie" }, "fetch_mode": "search"
    });
    let (st_m, b_m) = post_json(&env, "/api/v1/subscriptions", &[], m_body).await;
    let sub_rej: SubscribeId = assert_ok(st_m, &b_m, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    enqueue_and_run_search(&env, sub_rej)
        .await
        .expect("policy-rejected search.before round must succeed without site-failure error");
    assert!(
        !env.state
            .store()
            .load_wanted_history(sub_rej)
            .unwrap()
            .is_empty()
    );
    let run_u = format!("/api/v1/subscriptions/{sub_rej}/run");
    let (st_run, b_run) = post_json(&env, &run_u, &[], json!({})).await;
    assert_ok(st_run, &b_run, StatusCode::OK);

    let count_sb = || -> usize {
        env.state
            .store()
            .list_plugin_audit_events(pid, Some("search.before"), None, None, Some(200))
            .unwrap()
            .items
            .len()
    };
    let before = count_sb();
    let sub_p2 = create_tv_sub(&env, "醉梦", 3).await;
    let _ = enqueue_and_run_search(&env, sub_p2).await;
    assert_eq!(count_sb() - before, 6);
}
