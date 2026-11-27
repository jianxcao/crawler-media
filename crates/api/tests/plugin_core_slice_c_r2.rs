#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadSubmitError, FrozenDownloadPlan,
    Store, SubmissionPurpose, TorrentSource, router,
};
use axum::http::StatusCode;
use domain::{Media, MediaKind, PluginOperationId, Site, SiteId, SubscribeId, Torrent};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, MemoryDownloader, SubmitOptions,
    SubmitPayload, torrent_info_hash,
};
use indexer::RegistrySnapshot;
use jobs::Runner as _;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use serde_json::{Value, json};
use support::{StubSearchFetcher, StubTorrentServer, minimal_torrent_bytes, nexus_row};

fn spawn_one_shot_torrent(
    bytes: Vec<u8>,
    on_fetch: Option<Box<dyn FnOnce() + Send>>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.read(&mut [0u8; 2048]);
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
    addr
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

fn seed_site(env: &HttpEnv, name: &str, url: &str) -> SiteId {
    let id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: "demo".into(),
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

async fn create_tv_sub(
    env: &HttpEnv,
    title: &str,
    tmdb_id: Option<&str>,
    episode_to: Option<u32>,
    follow_future: bool,
    library_id: Option<&str>,
) -> SubscribeId {
    let mut media_obj = json!({ "kind": "tv", "title": title, "original_title": title });
    if let Some(tid) = tmdb_id {
        media_obj["tmdb_id"] = json!(tid);
    }
    let mut cov = json!({ "kind": "tv", "season": 1, "episode_from": 1 });
    if let Some(to) = episode_to {
        cov["episode_to"] = json!(to);
    }
    let mut body = json!({
        "media": media_obj,
        "coverage": cov,
        "fetch_mode": "search",
        "follow_future": follow_future
    });
    if let Some(lib_id) = library_id {
        body["library_id"] = json!(lib_id);
    }
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

async fn enqueue_and_run_search(env: &HttpEnv, sub_id: SubscribeId) -> Result<(), String> {
    let job = {
        let jobs = env.state.jobs();
        let q = jobs.acquire().unwrap();
        let payload = json!({ "subscribe_id": sub_id.to_string() }).to_string();
        q.enqueue(jobs::JobIdentity::subscribe_search(sub_id), &payload, 0)
            .unwrap();
        q.claim(store::unix_now() + 3600, 10)
            .unwrap()
            .into_iter()
            .find(|j| j.scope == jobs::JobScope::Subscribe(sub_id))
            .unwrap()
    };
    let st = env.state.clone();
    tokio::task::spawn_blocking(move || api::worker::StateRunner { state: st }.run(&job))
        .await
        .unwrap()
}

// --- Defect #2: finish_search library fencing at lines 74, 267, and 298 ---

struct HookedSeasonCatalog {
    calls: Arc<AtomicUsize>,
    on_call: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl api::catalog::Catalog for HookedSeasonCatalog {
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
    fn season_episodes(&self, _: &str, _: u32) -> Result<Vec<media::SeasonEpisode>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cb) = self.on_call.lock().unwrap().take() {
            cb();
        }
        Ok(vec![media::SeasonEpisode {
            episode_number: 1,
            air_date: Some("2020-01-01".into()),
        }])
    }
}

struct CancellingSubmitDl {
    inner: MemoryDownloader,
    on_submit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Downloader for CancellingSubmitDl {
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
}

#[tokio::test]
async fn finish_search_fences_cancelled_library_before_admit_catalog_and_wanted_round() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("Fence.Show.S01E01.1080p.WEB-DL", 4096);
    let srv = StubTorrentServer::start(t_bytes);
    let dl = Arc::new(CancellingSubmitDl {
        inner: MemoryDownloader::new(dir.path().join("stage")),
        on_submit: Mutex::new(None),
    });
    let html = nexus_row("Fence.Show.S01E01.1080p.WEB-DL", &srv.url("e01.torrent"));
    let mut env = make_custom_env(&dir, &html, dl.clone(), Some(srv.addr));
    let cat_calls = Arc::new(AtomicUsize::new(0));
    let cat = Arc::new(HookedSeasonCatalog {
        calls: Arc::clone(&cat_calls),
        on_call: Mutex::new(None),
    });
    env.state = env.state.with_catalog(cat.clone());
    env.app = router(env.state.clone());
    env.state
        .store()
        .put_setting(api::settings_keys::SUBSCRIBE_AIR_DATE_GATE, "false")
        .unwrap();
    seed_site(&env, "FenceSite", &srv.url(""));

    let mk_tv_lib = |name: &str| {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let root_str = root.path().to_str().unwrap().to_string();
        std::mem::forget(root);
        env.state
            .store()
            .create_library(MediaKind::Tv, name, &[&root_str], "everyone", false, &[])
            .unwrap()
            .id
    };

    // 1) Cancelled in-memory before admit_candidates (line 74): must abort before submit & catalog.
    let lib1 = mk_tv_lib("FenceLib1");
    let sub1 = create_tv_sub(&env, "Fence Show", Some("101"), None, false, Some(&lib1)).await;
    env.state.library_work().cancel(&lib1);
    assert!(enqueue_and_run_search(&env, sub1).await.is_err());
    assert!(dl.inner.submitted().is_empty());
    assert_eq!(cat_calls.load(Ordering::SeqCst), 0);
    assert!(
        env.state
            .store()
            .load_wanted_history(sub1)
            .unwrap()
            .is_empty()
    );

    // 2) Cancelled during submit_verified (line 267): must abort before catalog_aired_max & wanted.
    let lib2 = mk_tv_lib("FenceLib2");
    let sub2 = create_tv_sub(&env, "Fence Show", Some("102"), None, false, Some(&lib2)).await;
    let st2 = env.state.clone();
    let lib2_c = lib2.clone();
    *dl.on_submit.lock().unwrap() = Some(Box::new(move || st2.library_work().cancel(&lib2_c)));
    assert!(enqueue_and_run_search(&env, sub2).await.is_err());
    assert_eq!(cat_calls.load(Ordering::SeqCst), 0);
    assert!(
        env.state
            .store()
            .load_wanted_history(sub2)
            .unwrap()
            .is_empty()
    );

    // 3) Cancelled inside catalog_aired_max (line 298): must abort before record_wanted_round.
    let lib3 = mk_tv_lib("FenceLib3");
    let sub3 = create_tv_sub(&env, "NoHit Show", Some("103"), None, false, Some(&lib3)).await;
    let st3 = env.state.clone();
    let lib3_c = lib3.clone();
    *cat.on_call.lock().unwrap() = Some(Box::new(move || st3.library_work().cancel(&lib3_c)));
    assert!(enqueue_and_run_search(&env, sub3).await.is_err());
    assert_eq!(cat_calls.load(Ordering::SeqCst), 1);
    assert!(
        env.state
            .store()
            .load_wanted_history(sub3)
            .unwrap()
            .is_empty()
    );
}

// --- Defect #3a: /api/v1/search/admit SubmissionFence mid-fetch ---

#[tokio::test]
async fn search_admit_fences_mid_fetch_subscribe_change_before_downloader_submit() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let t_bytes = minimal_torrent_bytes("Admit.Fence.S01E01.1080p.WEB-DL", 4096);
    let mutate_slot = Arc::new(Mutex::new(None));
    let slot_c = Arc::clone(&mutate_slot);
    let addr = spawn_one_shot_torrent(
        t_bytes,
        Some(Box::new(move || {
            let (store, sub_id): (Arc<Store>, SubscribeId) = slot_c.lock().unwrap().take().unwrap();
            set_sub_season(&store, sub_id, 2);
        })),
    );
    let base = format!("http://127.0.0.1:{}/", addr.port());
    let enc = format!("{base}admit-fence.torrent");
    let html = nexus_row("Admit.Fence.S01E01.1080p.WEB-DL", &enc);
    let env = make_custom_env(&dir, &html, dl.clone(), Some(addr));
    seed_site(&env, "AdmitFenceSite", &base);
    let sub_id = create_tv_sub(&env, "Admit Fence", None, Some(2), true, None).await;
    *mutate_slot.lock().unwrap() = Some((env.state.store(), sub_id));

    let body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc });
    let (st, b) = post_json(&env, "/api/v1/search/admit", &[], body).await;
    assert_err(st, &b, StatusCode::CONFLICT, "subscription.changed");
    assert!(
        dl.submitted().is_empty(),
        "SubmissionFence must abort before submit_verified when subscribe changed mid-fetch"
    );
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());
}

// --- Defect #3b & #3c: /api/v1/search/admit rollback by hash, #rb skip, pending restore ---

struct MutatingAdmitDownloader {
    inner: MemoryDownloader,
    mutate_sub: Mutex<Option<(Arc<Store>, SubscribeId)>>,
}

impl Downloader for MutatingAdmitDownloader {
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
async fn search_admit_race_rollback_deletes_by_hash_skips_rolled_back_idem_and_restores_pending() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("Admit.Show.S01E01.1080p.WEB-DL", 4096);
    let expected_hash = torrent_info_hash(&t_bytes).unwrap();
    let srv = StubTorrentServer::start(t_bytes);
    let dl = Arc::new(MutatingAdmitDownloader {
        inner: MemoryDownloader::new(dir.path().join("stage")),
        mutate_sub: Mutex::new(None),
    });
    let enc = srv.url("admit-s01e01.torrent");
    let html = nexus_row("Admit.Show.S01E01.1080p.WEB-DL", &enc);
    let env = make_custom_env(&dir, &html, dl.clone(), Some(srv.addr));
    seed_site(&env, "AdmitRbSite", &srv.url(""));
    let sub_id = create_tv_sub(&env, "Admit Show", None, Some(2), true, None).await;

    *dl.mutate_sub.lock().unwrap() = Some((env.state.store(), sub_id));
    let body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc });
    let hdr = [("idempotency-key", "admit-rb-idem-1")];

    let (st1, b1) = post_json(&env, "/api/v1/search/admit", &hdr, body.clone()).await;
    assert_err(st1, &b1, StatusCode::CONFLICT, "subscription.changed");
    assert_eq!(dl.inner.deleted_tasks(), vec![(expected_hash, false)]);
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());

    set_sub_season(env.state.store().as_ref(), sub_id, 1);

    let (st2, b2) = post_json(&env, "/api/v1/search/admit", &hdr, body.clone()).await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(
        dl.inner.submitted().len(),
        2,
        "retry with same Idempotency-Key must skip rolled-back op and re-submit"
    );
    let pending = env.state.store().load_pending(sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());

    env.state.store().delete_pending(sub_id, &enc).unwrap();
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());

    let (st3, b3) = post_json(&env, "/api/v1/search/admit", &hdr, body).await;
    assert_ok(st3, &b3, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 2);
    let restored = env.state.store().load_pending(sub_id).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].1.submitted_at.is_some());
}

// --- Defect #3d: Unknown manual Idempotency-Key replay short-circuit ---

struct AlwaysUnknownDl(AtomicUsize);
impl Downloader for AlwaysUnknownDl {
    fn submit_verified(
        &self,
        _: &SubmitPayload,
        _: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ClientSubmitResult::UnknownOutcome {
            reason: "rpc timeout".into(),
        })
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn manual_unknown_idem_replay_short_circuits_without_rewriting_subscribe_db() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("Unk.Show.S01E01.1080p", 4096));
    let dl = Arc::new(AlwaysUnknownDl(AtomicUsize::new(0)));
    let env = make_custom_env(&dir, "", dl.clone(), Some(srv.addr));
    let site_id = seed_site(&env, "UnkSite", &srv.url(""));
    let sub_id = create_tv_sub(&env, "Unk Show", None, Some(2), true, None).await;

    let body = json!({
        "download_url": srv.url("unk-s01e01.torrent"),
        "title": "Unk.Show.S01E01.1080p",
        "site_id": site_id.to_string(),
        "subscribe_id": sub_id.to_string(),
    });
    let hdr = [("idempotency-key", "manual-unk-idem-1")];
    let (st1, b1) = post_json(&env, "/api/v1/downloaders/submit", &hdr, body.clone()).await;
    assert_ok(st1, &b1, StatusCode::OK);
    assert_eq!(dl.0.load(Ordering::SeqCst), 1);
    let p1 = env.state.store().load_pending(sub_id).unwrap();
    assert_eq!(p1.len(), 1);
    assert!(p1[0].1.submitted_at.is_none());

    // Hold subscribe_guard on another thread so any call to persist_or_rollback_pending would block!
    let guard = env.state.subscribe_guard(sub_id);
    let released = Arc::new(AtomicBool::new(false));
    let rel_c = Arc::clone(&released);
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let locker = std::thread::spawn(move || {
        let _g = guard.lock();
        let _ = locked_tx.send(());
        while !rel_c.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    locked_rx.recv().unwrap();

    let replay_res = tokio::time::timeout(
        Duration::from_millis(400),
        post_json(&env, "/api/v1/downloaders/submit", &hdr, body),
    )
    .await;
    released.store(true, Ordering::SeqCst);
    locker.join().unwrap();

    let (st2, b2) =
        replay_res.expect("Unknown replay must short-circuit without locking subscribe_guard");
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(dl.0.load(Ordering::SeqCst), 1);
}

// --- Defect #4: check_submission_fence propagates ReadBusy & cancels missing default library ---

fn build_manual_plan_and_ctx(
    env: &HttpEnv,
    srv: &StubTorrentServer,
    site_id: SiteId,
    sub_id: SubscribeId,
    tag: &str,
) -> (FrozenDownloadPlan, DownloadOpContext) {
    let sub = env.state.store().get_subscribe(sub_id).unwrap().unwrap();
    let url = srv.url(&format!("{tag}.torrent"));
    let torrent = Torrent {
        site_id,
        id: Some("1".into()),
        title: "Busy.Fence.S01E01.1080p".into(),
        enclosure: url.clone(),
        size_bytes: Some(4096),
        seeders: Some(10),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: false,
        hr: None,
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    };
    let plan = FrozenDownloadPlan {
        torrent,
        source: TorrentSource::from_url(&url).unwrap(),
        purpose: SubmissionPurpose::Manual,
        downloader_id: None,
        frozen_endpoint: None,
        subscribe_id: Some(sub_id),
        subscribe_generation: None,
        policy_revision: None,
        media_id: Some(sub.media_id),
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
        cause_id: format!("test:{op_id}"),
        subject: "user:test".into(),
        idempotency_key: format!("idem:{tag}"),
        plugin_id: None,
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, ctx)
}

#[tokio::test]
async fn check_submission_fence_propagates_read_busy_and_cancels_missing_default_library() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("Busy.Fence.1999.1080p", 4096));
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_custom_env(&dir, "", dl.clone(), Some(srv.addr));
    let site_id = seed_site(&env, "BusySite", &srv.url(""));
    let sub_default = create_tv_sub(&env, "Busy Default", None, Some(2), true, None).await;
    let mut sub_row = env
        .state
        .store()
        .get_subscribe(sub_default)
        .unwrap()
        .unwrap();
    sub_row.library_id = None;
    env.state.store().update_subscribe(&sub_row).unwrap();
    let tv_lib = env
        .state
        .store()
        .default_library(MediaKind::Tv)
        .unwrap()
        .unwrap()
        .id;
    let sub_explicit =
        create_tv_sub(&env, "Busy Explicit", None, Some(2), true, Some(&tv_lib)).await;

    let pool = env.state.store().reader_pool(store::Db::Library);
    let mut held = Vec::new();
    while let Ok(lease) = pool.checkout(Duration::ZERO) {
        held.push(lease);
    }
    assert!(!held.is_empty());

    let svc = ApiDownloadService::new(env.state.clone());
    let (p_def, c_def) = build_manual_plan_and_ctx(&env, &srv, site_id, sub_default, "def-busy");
    let mut inner_def = None;
    let _ = env
        .state
        .store()
        .with_read_scope(Duration::from_millis(25), || {
            inner_def = Some(svc.submit_sync(p_def, c_def));
        });
    let res_def = inner_def.unwrap();
    assert!(
        matches!(res_def, Err(DownloadSubmitError::Busy(_))),
        "default-library ReadBusy must return Busy, got {res_def:?}"
    );
    assert!(dl.submitted().is_empty());

    let (p_exp, c_exp) = build_manual_plan_and_ctx(&env, &srv, site_id, sub_explicit, "exp-busy");
    let mut inner_exp = None;
    let _ = env
        .state
        .store()
        .with_read_scope(Duration::from_millis(25), || {
            inner_exp = Some(svc.submit_sync(p_exp, c_exp));
        });
    let res_exp = inner_exp.unwrap();
    assert!(
        matches!(res_exp, Err(DownloadSubmitError::Busy(_))),
        "explicit-library ReadBusy must return Busy (not Cancelled), got {res_exp:?}"
    );
    drop(held);

    {
        let conn = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
        conn.execute("UPDATE libraries SET is_default = 0 WHERE kind = 'tv'", [])
            .unwrap();
    }
    assert!(
        env.state
            .store()
            .default_library(MediaKind::Tv)
            .unwrap()
            .is_none()
    );

    let (p_none, c_none) = build_manual_plan_and_ctx(&env, &srv, site_id, sub_default, "def-none");
    let res_none = svc.submit_sync(p_none, c_none);
    assert!(
        matches!(res_none, Err(DownloadSubmitError::Cancelled)),
        "missing default_library must return Cancelled, got {res_none:?}"
    );
    assert!(dl.submitted().is_empty());
}
