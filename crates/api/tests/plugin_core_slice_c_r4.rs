#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadOutcome, FrozenDownloadPlan, Store,
    SubmissionPurpose, TorrentSource, router,
};
use axum::http::StatusCode;
use domain::{DownloaderId, PluginOperationId, Site, SiteId, SubscribeId, Torrent};
use downloader::{
    ClientSubmitResult, ClientTaskQuery, Downloader, DownloaderError, MemoryDownloader,
    SubmitOptions, SubmitPayload, TaskSnapshot, torrent_info_hash,
};
use indexer::RegistrySnapshot;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use serde_json::{Value, json};
use store::{DownloadOperationState, DownloadOwnershipState};
use support::{StubSearchFetcher, StubTorrentServer, minimal_torrent_bytes, nexus_row};
use uuid::Uuid;

fn make_env(
    dir: &tempfile::TempDir,
    payload: &str,
    dl: Arc<dyn Downloader>,
    addrs: &[SocketAddr],
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

async fn req_authed(env: &HttpEnv, method: &str, uri: &str) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json(method, uri, Some(TOKEN), &[], None))
        .await;
    (st, b)
}

async fn create_tv_sub(env: &HttpEnv, title: &str, library_id: Option<&str>) -> SubscribeId {
    let cov = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let media = json!({ "kind": "tv", "title": title, "original_title": title });
    let mut body = json!({
        "media": media,
        "coverage": cov,
        "fetch_mode": "search",
        "follow_future": false
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

fn build_plan_and_ctx(
    (site_id, url, title): (SiteId, &str, &str),
    purpose: SubmissionPurpose,
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
        free_2x: false,
        free_until: None,
    };
    let plan = FrozenDownloadPlan {
        torrent,
        source: TorrentSource::from_url(url).unwrap(),
        purpose,
        downloader_id: None,
        frozen_endpoint: None,
        subscribe_id: None,
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
        cause_id: format!("r4:{op_id}"),
        subject: "user:r4".into(),
        idempotency_key: idem_key.into(),
        plugin_id: plugin_id.map(str::to_string),
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, ctx)
}

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

struct HookedMemoryDownloader {
    inner: MemoryDownloader,
    identities: Mutex<HashMap<String, String>>,
    on_submit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    on_delete: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl HookedMemoryDownloader {
    fn new(stage: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryDownloader::new(stage),
            identities: Mutex::new(HashMap::new()),
            on_submit: Mutex::new(None),
            on_delete: Mutex::new(None),
        })
    }
}

impl Downloader for HookedMemoryDownloader {
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
    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        self.inner.query_task_status(info_hash, ownership_tag)
    }
    fn completed_files(&self, t: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.inner.completed_files(t)
    }
    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        let remembered = self.identities.lock().unwrap().remove(&torrent.enclosure);
        if let Some(hash) = remembered {
            self.inner.delete_task(&hash, delete_files)?;
        }
        self.inner.remove_owned(torrent, delete_files)
    }
    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        if let Some(cb) = self.on_delete.lock().unwrap().take() {
            cb();
        }
        self.inner.delete_task(info_hash, delete_files)
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Ok(vec![])
    }
}

// --- Item 1: Reconciling Unknown op that reused existing_owned_tag ---

#[tokio::test]
async fn reconciling_unknown_op_reusing_existing_owned_tag_submits_without_foreign_or_fk_error() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("Boost.Reconcile.1999.1080p", 4096);
    let expected_hash = torrent_info_hash(&t_bytes).unwrap();
    let srv = StubTorrentServer::start(t_bytes);
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, "", dl.clone(), &[srv.addr]);
    let site_id = seed_site(&env, "ReconcileSite", &srv.url(""));
    let url = srv.url("reconcile-dedup.torrent");
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());

    let (m_plan, m_ctx) = build_plan_and_ctx(
        (site_id, &url, "Boost.Reconcile.1999.1080p"),
        SubmissionPurpose::Manual,
        None,
        "idem-r4-manual-1",
    );
    let first_tag = format!("cm-op:{}", m_ctx.operation_id);
    let svc = ApiDownloadService::new(env.state.clone());
    svc.submit_sync(m_plan, m_ctx).unwrap();

    dl.set_force_unknown(true);
    let (b_plan, b_ctx) = build_plan_and_ctx(
        (site_id, &url, "Boost.Reconcile.1999.1080p"),
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "idem-r4-boost-2",
    );
    let second_tag = format!("cm-op:{}", b_ctx.operation_id);
    let second_op_id = b_ctx.operation_id.to_string();
    let out2 = svc.submit_sync(b_plan, b_ctx).unwrap();
    assert!(matches!(out2, DownloadOutcome::Unknown { .. }));
    assert!(
        env.state
            .store()
            .get_download_ownership(nil_dl, &second_tag)
            .unwrap()
            .is_none(),
        "record_unknown_outcome must skip Unproven row when reusing existing_owned_tag"
    );

    dl.set_force_unknown(false);
    api::recover_plugin_startup_for_test(&env.state).unwrap();

    let op2 = env
        .state
        .store()
        .get_download_operation(&second_op_id)
        .unwrap()
        .unwrap();
    assert_eq!(op2.state, DownloadOperationState::Submitted);
    assert_eq!(op2.outcome_code.as_deref(), Some("reconciled_submitted"));

    let rows = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &expected_hash)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ownership_tag, first_tag);
    assert_eq!(rows[0].ownership_state, DownloadOwnershipState::Owned);

    let facts = env.state.store().list_active_boost_task_facts().unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].ownership_tag, first_tag);
}

// --- Item 2: /api/v1/search/admit post-submit ReadBusy rollback, Cancelled, and Unknown guards ---

#[tokio::test]
async fn search_admit_post_submit_read_busy_rolls_back_task_and_ownership_returning_503() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("Admit.Busy.S01E01.1080p.WEB-DL", 4096);
    let expected_hash = torrent_info_hash(&t_bytes).unwrap();
    let srv = StubTorrentServer::start(t_bytes);
    let dl = HookedMemoryDownloader::new(dir.path().join("stage"));
    let enc = srv.url("admit-busy.torrent");
    let html = nexus_row("Admit.Busy.S01E01.1080p.WEB-DL", &enc);
    let env = make_env(&dir, &html, dl.clone(), &[srv.addr]);
    seed_site(&env, "AdmitBusySite", &srv.url(""));
    let sub_id = create_tv_sub(&env, "Admit Busy", None).await;
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());

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

    let body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc });
    let hdr = [("idempotency-key", "admit-r4-busy-1")];
    let (st1, b1) = post_json(&env, "/api/v1/search/admit", &hdr, body.clone()).await;
    held_leases.lock().unwrap().clear();
    assert_err(st1, &b1, StatusCode::SERVICE_UNAVAILABLE, "runtime.busy");
    assert_eq!(
        dl.inner.deleted_tasks(),
        vec![(expected_hash.clone(), false)]
    );
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());
    let own = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &expected_hash)
        .unwrap();
    assert_eq!(own[0].ownership_state, DownloadOwnershipState::Deleted);

    let (st2, b2) = post_json(&env, "/api/v1/search/admit", &hdr, body).await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 2);
}

#[tokio::test]
async fn search_admit_unknown_and_cancelled_guard_subscribe_drift_and_clean_pending() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t1 = minimal_torrent_bytes("Admit.Guard.S01E01.1080p.WEB-DL", 4096);
    let t2 = minimal_torrent_bytes("Admit.Guard.S01E02.1080p.WEB-DL", 4096);
    let srv1 = StubTorrentServer::start(t1);
    let mutate_fetch = Arc::new(Mutex::new(None::<Box<dyn FnOnce() + Send>>));
    let mf_c = Arc::clone(&mutate_fetch);
    let addr2 = spawn_one_shot_torrent(
        t2,
        Some(Box::new(move || {
            if let Some(cb) = mf_c.lock().unwrap().take() {
                cb();
            }
        })),
    );
    let enc1 = srv1.url("e01.torrent");
    let enc2 = format!("http://127.0.0.1:{}/e02.torrent", addr2.port());
    let html = format!(
        "{}{}",
        nexus_row("Admit.Guard.S01E01.1080p.WEB-DL", &enc1),
        nexus_row("Admit.Guard.S01E02.1080p.WEB-DL", &enc2)
    );
    let dl = HookedMemoryDownloader::new(dir.path().join("stage"));
    let env = make_env(&dir, &html, dl.clone(), &[srv1.addr, addr2]);
    seed_site(&env, "GuardSite1", &srv1.url(""));
    seed_site(
        &env,
        "GuardSite2",
        &format!("http://127.0.0.1:{}/", addr2.port()),
    );

    let lib_dir = tempfile::tempdir_in("/tmp").unwrap();
    let lib_id = env
        .state
        .store()
        .create_library(
            domain::MediaKind::Tv,
            "GuardLib",
            &[lib_dir.path().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id;
    let sub_id = create_tv_sub(&env, "Admit Guard", Some(&lib_id)).await;

    // (a) DownloadOutcome::Unknown with concurrent subscription drift -> 409 + clean pending.
    dl.inner.set_force_unknown(true);
    let st_unk = env.state.store();
    *dl.on_submit.lock().unwrap() = Some(Box::new(move || set_sub_season(&st_unk, sub_id, 2)));
    let b1 = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc1 });
    let (s1, r1) = post_json(&env, "/api/v1/search/admit", &[], b1).await;
    assert_err(s1, &r1, StatusCode::CONFLICT, "subscription.changed");
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());
    dl.inner.set_force_unknown(false);
    set_sub_season(env.state.store().as_ref(), sub_id, 1);

    // (b) DownloadSubmitError::Cancelled where &cur != subscribe (follow_future changed) -> 409 subscription.changed.
    let st_can = env.state.clone();
    let lib_c = lib_id.clone();
    *mutate_fetch.lock().unwrap() = Some(Box::new(move || {
        let mut s = st_can.store().get_subscribe(sub_id).unwrap().unwrap();
        s.follow_future = true;
        st_can.store().update_subscribe(&s).unwrap();
        st_can.library_work().cancel(&lib_c);
    }));
    let b2 = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc2 });
    let (s2, r2) = post_json(&env, "/api/v1/search/admit", &[], b2).await;
    assert_err(s2, &r2, StatusCode::CONFLICT, "subscription.changed");
    assert!(env.state.store().load_pending(sub_id).unwrap().is_empty());
}

// --- Item 3a: DELETE /api/v1/downloaders/tasks/{hash} marks ownership Deleted ---

#[tokio::test]
async fn delete_task_marks_ownership_deleted_hides_missing_row_and_allows_idem_resubmit() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t_bytes = minimal_torrent_bytes("Manual.Delete.2025.1080p", 4096);
    let expected_hash = torrent_info_hash(&t_bytes).unwrap();
    let srv = StubTorrentServer::start(t_bytes);
    let dl = HookedMemoryDownloader::new(dir.path().join("stage"));
    let env = make_env(&dir, "", dl.clone(), &[srv.addr]);
    let site_id = seed_site(&env, "ManualDelSite", &srv.url(""));
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());

    let submit_body = json!({
        "download_url": srv.url("man-del.torrent"),
        "title": "Manual.Delete.2025.1080p",
        "site_id": site_id.to_string(),
    });
    let hdr = [("idempotency-key", "man-del-idem-1")];
    let (st1, b1) = post_json(
        &env,
        "/api/v1/downloaders/submit",
        &hdr,
        submit_body.clone(),
    )
    .await;
    assert_ok(st1, &b1, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 1);
    let (st_before, b_before) = req_authed(&env, "GET", "/api/v1/downloaders/tasks").await;
    assert_eq!(
        assert_ok(st_before, &b_before, StatusCode::OK)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let (st_del, b_del) = req_authed(
        &env,
        "DELETE",
        &format!("/api/v1/downloaders/tasks/{expected_hash}"),
    )
    .await;
    assert_ok(st_del, &b_del, StatusCode::OK);

    let own = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &expected_hash)
        .unwrap();
    assert_eq!(own[0].ownership_state, DownloadOwnershipState::Deleted);

    let (st_list, b_list) = req_authed(&env, "GET", "/api/v1/downloaders/tasks").await;
    let listed = assert_ok(st_list, &b_list, StatusCode::OK);
    assert_eq!(listed["items"].as_array().unwrap().len(), 0);

    // Deleting a foreign/legacy hash with no ownership row still succeeds normally.
    let (st_leg, b_leg) = req_authed(
        &env,
        "DELETE",
        "/api/v1/downloaders/tasks/0123456789abcdef0123456789abcdef01234567",
    )
    .await;
    assert_ok(st_leg, &b_leg, StatusCode::OK);

    // Re-submitting with the same Idempotency-Key skips the deleted op and submits fresh.
    let (st2, b2) = post_json(&env, "/api/v1/downloaders/submit", &hdr, submit_body).await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 2);
}

// --- Item 3b: POST /api/v1/downloaders/tasks/remove marks ownership Deleted ---

#[tokio::test]
async fn remove_task_marks_ownership_deleted_and_allows_idem_resubmit_for_submit_and_admit() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let t1 = minimal_torrent_bytes("Sub.Remove.S01E01.1080p.WEB-DL", 4096);
    let t2 = minimal_torrent_bytes("Sub.Remove.S01E02.1080p.WEB-DL", 4096);
    let (h1, h2) = (
        torrent_info_hash(&t1).unwrap(),
        torrent_info_hash(&t2).unwrap(),
    );
    let (srv1, srv2) = (StubTorrentServer::start(t1), StubTorrentServer::start(t2));
    let (enc1, enc2) = (srv1.url("rm-e01.torrent"), srv2.url("rm-e02.torrent"));
    let html = nexus_row("Sub.Remove.S01E02.1080p.WEB-DL", &enc2);
    let dl = HookedMemoryDownloader::new(dir.path().join("stage"));
    let env = make_env(&dir, &html, dl.clone(), &[srv1.addr, srv2.addr]);
    let site1 = seed_site(&env, "SubRmSite1", &srv1.url(""));
    seed_site(&env, "SubRmSite2", &srv2.url(""));
    let sub_id = create_tv_sub(&env, "Sub Remove", None).await;
    let nil_dl = DownloaderId::from_uuid(Uuid::nil());

    // 1) Manual /downloaders/submit with subscribe_id -> tasks/remove -> re-submit same Idempotency-Key.
    let sub_body = json!({
        "download_url": enc1,
        "title": "Sub.Remove.S01E01.1080p.WEB-DL",
        "site_id": site1.to_string(),
        "subscribe_id": sub_id.to_string(),
    });
    let hdr1 = [("idempotency-key", "sub-rm-submit-1")];
    let (s1, b1) = post_json(&env, "/api/v1/downloaders/submit", &hdr1, sub_body.clone()).await;
    assert_ok(s1, &b1, StatusCode::OK);
    let rm1 = json!({ "task_id": format!("{sub_id}:{enc1}"), "delete_files": false });
    let (sr1, br1) = post_json(&env, "/api/v1/downloaders/tasks/remove", &[], rm1).await;
    assert_ok(sr1, &br1, StatusCode::OK);
    let own1 = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &h1)
        .unwrap();
    assert_eq!(own1[0].ownership_state, DownloadOwnershipState::Deleted);
    let (s1_re, b1_re) = post_json(&env, "/api/v1/downloaders/submit", &hdr1, sub_body).await;
    assert_ok(s1_re, &b1_re, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 2);

    // 2) /search/admit -> tasks/remove -> re-admit same Idempotency-Key.
    let admit_body = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc2 });
    let hdr2 = [("idempotency-key", "sub-rm-admit-2")];
    let (s2, b2) = post_json(&env, "/api/v1/search/admit", &hdr2, admit_body.clone()).await;
    assert_ok(s2, &b2, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 3);
    let rm2 = json!({ "task_id": format!("{sub_id}:{enc2}"), "delete_files": false });
    let (sr2, br2) = post_json(&env, "/api/v1/downloaders/tasks/remove", &[], rm2).await;
    assert_ok(sr2, &br2, StatusCode::OK);
    let own2 = env
        .state
        .store()
        .find_download_ownership_by_hash(nil_dl, &h2)
        .unwrap();
    assert_eq!(own2[0].ownership_state, DownloadOwnershipState::Deleted);
    let (s2_re, b2_re) = post_json(&env, "/api/v1/search/admit", &hdr2, admit_body).await;
    assert_ok(s2_re, &b2_re, StatusCode::OK);
    assert_eq!(dl.inner.submitted().len(), 4);
}
