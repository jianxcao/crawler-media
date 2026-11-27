use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, SubscribeId};
use downloader::{
    ClientSubmitResult, Downloader, DownloaderError, SubmitOptions, SubmitPayload, TaskSnapshot,
};
use indexer::RegistrySnapshot;
use serde_json::json;
use store::RecordSubscribeDeferInput;

use super::plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use super::support::{
    StubSearchFetcher, StubTorrentServer, enable_hooks_plugin, minimal_torrent_bytes, nexus_row,
    seed_site_and_subscribe, setup_subscribe_env, spawn_tr_add_stub,
};

#[tokio::test]
async fn subscribe_runs_filter_after_and_subscribe_score_with_shared_budget() {
    let ctx = setup_subscribe_env("The.Matrix.OverBudget.1999.1080p", "matrix.torrent").await;
    enable_hooks_plugin(
        &ctx.env,
        "org.example.sub-filter-after",
        &[
            "filter.after",
            "subscribe.score",
            "subscribe.select",
            "download.before",
            "download.after",
        ],
    )
    .await;

    let (st_run, _, b_run) = ctx
        .env
        .send(req_json(
            "POST",
            &format!("/api/v1/subscriptions/{}/run", ctx.sub_id),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_run, &b_run, StatusCode::OK);

    let (st_act, _, b_act) = ctx
        .env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{}/activities", ctx.sub_id),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let act = assert_ok(st_act, &b_act, StatusCode::OK);
    let items = act["items"].as_array().unwrap();
    let find_step = |hook_id: &str| {
        items
            .iter()
            .find(|i| {
                i["type"].as_str() == Some("hook_trace") && i["kind"].as_str() == Some(hook_id)
            })
            .and_then(|i| i["plugin_trace"]["plugin_steps"].as_array())
            .and_then(|s| s.first())
            .and_then(|s| s["status"].as_str())
            .map(str::to_string)
    };
    assert_eq!(find_step("filter.after").as_deref(), Some("applied"));
    assert_eq!(find_step("subscribe.score").as_deref(), Some("rolled_back"));
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, 88, "base 80 + filter.after 8; +5 rolled back");
}

#[tokio::test]
async fn activities_redacts_job_error_and_defer_secrets() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p", "matrix.torrent").await;
    {
        let jobs = ctx.env.state.jobs();
        let q = jobs.acquire().unwrap();
        let payload = json!({ "subscribe_id": ctx.sub_id.to_string() }).to_string();
        let job = q
            .enqueue(jobs::JobIdentity::subscribe_search(ctx.sub_id), &payload, 0)
            .unwrap();
        let claimed = q.claim(0, 10).unwrap();
        assert!(claimed.iter().any(|c| c.id == job.id));
        q.fail(
            job.id,
            1,
            "upstream error at https://pt.example/api?passkey=JOB_LEAK_123&pass=JOB_PASS_456",
        )
        .unwrap();
    }
    let binding = ctx
        .env
        .state
        .store()
        .subscribe_policy_binding(ctx.sub_id)
        .unwrap()
        .unwrap();
    ctx.env
        .state
        .store()
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: uuid::Uuid::new_v4().to_string(),
            subscribe_id: ctx.sub_id,
            subscribe_generation: binding.jobs_sync_generation,
            policy_revision: binding.policy_revision,
            enclosure: "https://pt.example/d.torrent?id=9&pass=ENC_PASS_789".into(),
            info_hash: None,
            plugin_id: Some("org.example.sub".into()),
            hook_id: "download.before".into(),
            reason: "Cookie: uid=1; pass=SECRET_DEFER_PASS".into(),
            next_retry_at: 0,
            now_utc: 100,
        })
        .unwrap();

    let (st, _, body) = ctx
        .env
        .send(req_json(
            "GET",
            &format!("/api/v1/subscriptions/{}/activities", ctx.sub_id),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK);
    let text = body.to_string();
    for secret in [
        "JOB_LEAK_123",
        "JOB_PASS_456",
        "SECRET_DEFER_PASS",
        "ENC_PASS_789",
        "uid=1",
    ] {
        assert!(!text.contains(secret), "leaked {secret} in {text}");
    }
}

struct MidflightDeleteDownloader {
    state: Arc<Mutex<Option<ApiState>>>,
    sub_id: Arc<Mutex<Option<SubscribeId>>>,
    already_owned: AtomicBool,
    removed_count: AtomicUsize,
}

impl Downloader for MidflightDeleteDownloader {
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        _opts: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        let st = self.state.lock().unwrap().clone().unwrap();
        let id = self.sub_id.lock().unwrap().unwrap();
        st.mark_subscribe_deleting(id);
        let _ = st.store().delete_subscribe(id);
        Ok(ClientSubmitResult::Added {
            already_owned: self.already_owned.load(Ordering::SeqCst),
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn remove_owned(&self, _t: &domain::Torrent, _del: bool) -> Result<(), DownloaderError> {
        let st = self.state.lock().unwrap().clone().unwrap();
        let id = self.sub_id.lock().unwrap().unwrap();
        assert!(
            st.subscribe_guard(id).try_lock().is_some(),
            "subscribe_guard must NOT be held during remove_owned"
        );
        self.removed_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(vec![])
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn submission_midflight_delete_returns_409_and_drops_guard_before_remove_owned() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let srv = StubTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 2048));
    let dl = Arc::new(MidflightDeleteDownloader {
        state: Arc::new(Mutex::new(None)),
        sub_id: Arc::new(Mutex::new(None)),
        already_owned: AtomicBool::new(false),
        removed_count: AtomicUsize::new(0),
    });
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubSearchFetcher {
            payload: String::new(),
        }),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr),
    );
    *dl.state.lock().unwrap() = Some(state.clone());
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state),
        member_token: String::new(),
    };
    let (site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    *dl.sub_id.lock().unwrap() = Some(sub_id);

    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &[],
            Some(json!({
                "download_url": srv.url("submit.torrent"),
                "title": "The.Matrix.1999.2160p",
                "site_id": site_id.to_string(),
                "subscribe_id": sub_id.to_string(),
                "force": true
            })),
        ))
        .await;
    assert_err(st, &body, StatusCode::CONFLICT, "subscription.deleting");
    assert_eq!(dl.removed_count.load(Ordering::SeqCst), 1);

    let (_site2, sub2) = seed_site_and_subscribe(&env, &srv.url("")).await;
    *dl.sub_id.lock().unwrap() = Some(sub2);
    dl.already_owned.store(true, Ordering::SeqCst);
    let (st2, _, b2) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &[],
            Some(json!({
                "download_url": srv.url("submit.torrent"),
                "title": "The.Matrix.1999.2160p",
                "site_id": site_id.to_string(),
                "subscribe_id": sub2.to_string(),
                "force": true
            })),
        ))
        .await;
    assert_err(st2, &b2, StatusCode::CONFLICT, "subscription.deleting");
    assert_eq!(
        dl.removed_count.load(Ordering::SeqCst),
        1,
        "already_owned must not remove_owned"
    );
}

#[tokio::test]
async fn admit_cleans_up_unconfirmed_pending_and_persists_patched_downloader_and_allows_retry() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p.BluRay.x264", "admit.torrent").await;
    let dl_b = DownloaderId::from_uuid(uuid::Uuid::from_u128(
        0xbbbb_bbbb_bbbb_4bbb_8bbb_bbbb_bbbb_bbbb,
    ));
    ctx.env
        .state
        .store()
        .insert_downloader(&api::store::DownloaderRow {
            id: dl_b,
            name: "secondary".into(),
            kind: "transmission".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: true,
        })
        .unwrap();

    *ctx.srv.status_override.lock().unwrap() = 404;
    let url = ctx.srv.url("admit.torrent");
    let (st_err, _, _) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &[],
            Some(json!({ "subscribe_id": ctx.sub_id.to_string(), "enclosure": url })),
        ))
        .await;
    assert_eq!(st_err, StatusCode::BAD_REQUEST);
    assert!(
        ctx.env
            .state
            .store()
            .load_pending(ctx.sub_id)
            .unwrap()
            .is_empty(),
        "failed admit must clean up unconfirmed pending row"
    );

    *ctx.srv.status_override.lock().unwrap() = 200;
    let (st_ok, _, b_ok) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &[],
            Some(json!({ "subscribe_id": ctx.sub_id.to_string(), "enclosure": url })),
        ))
        .await;
    assert_ok(st_ok, &b_ok, StatusCode::OK);
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].1.submitted_at.is_some());

    let ctx_defer = setup_subscribe_env("The.Matrix.DeferDownload.1080p", "defer.torrent").await;
    enable_hooks_plugin(
        &ctx_defer.env,
        "org.example.admit-defer",
        &["download.before"],
    )
    .await;
    let (st_def, _, b_def) = ctx_defer
        .env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &[],
            Some(json!({
                "subscribe_id": ctx_defer.sub_id.to_string(),
                "enclosure": ctx_defer.srv.url("defer.torrent")
            })),
        ))
        .await;
    assert_err(st_def, &b_def, StatusCode::CONFLICT, "plugin.deferred");
    assert!(
        ctx_defer
            .env
            .state
            .store()
            .load_pending(ctx_defer.sub_id)
            .unwrap()
            .is_empty(),
        "deferred admit must clean up unconfirmed pending row"
    );
}

struct UnknownAndHashDownloader {
    unknown_mode: AtomicBool,
    returned_hash: Mutex<String>,
}

impl Downloader for UnknownAndHashDownloader {
    fn submit_verified(
        &self,
        _payload: &SubmitPayload,
        _opts: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if self.unknown_mode.load(Ordering::SeqCst) {
            Ok(ClientSubmitResult::UnknownOutcome {
                reason: "network timeout".into(),
            })
        } else {
            Ok(ClientSubmitResult::Added {
                already_owned: false,
                info_hash: self.returned_hash.lock().unwrap().clone(),
            })
        }
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(vec![])
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn manual_subscribe_run_persists_unknown_pending_and_submitted_hash() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let srv = StubTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 4096));
    let dl = Arc::new(UnknownAndHashDownloader {
        unknown_mode: AtomicBool::new(true),
        returned_hash: Mutex::new("0123456789abcdef0123456789abcdef01234567".into()),
    });
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubSearchFetcher {
            payload: nexus_row("The.Matrix.1999.1080p.BluRay", "unknown.torrent"),
        }),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr),
    );
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state),
        member_token: String::new(),
    };
    let (_site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/subscriptions/{sub_id}/run"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK);
    let pending = env.state.store().load_pending(sub_id).unwrap();
    assert_eq!(pending.len(), 1, "Unknown run must persist pending row");
    assert!(pending[0].1.submitted_at.is_none());
    assert!(pending[0].1.downloader_id.is_some());

    let (_site2, sub2) = seed_site_and_subscribe(&env, &srv.url("")).await;
    dl.unknown_mode.store(false, Ordering::SeqCst);
    let (st2, _, b2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/subscriptions/{sub2}/run"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::OK);
    let hashes = env.state.store().load_pending_info_hashes(sub2).unwrap();
    assert_eq!(
        hashes.get(&srv.url("unknown.torrent")).map(String::as_str),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
}

struct CountingSubmissionDl {
    calls: AtomicUsize,
}

impl Downloader for CountingSubmissionDl {
    fn submit_verified(
        &self,
        _payload: &SubmitPayload,
        _opts: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ClientSubmitResult::Added {
            already_owned: false,
            info_hash: format!("{idx:040x}"),
        })
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(vec![])
    }
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Ok(vec![])
    }
}

fn read_full_http_request(s: &mut std::net::TcpStream) -> String {
    use std::io::Read;
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

fn spawn_transmission_add_stub() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let add_calls = Arc::new(AtomicUsize::new(0));
    let add_c = Arc::clone(&add_calls);
    std::thread::spawn(move || {
        while let Ok((mut s, _)) = listener.accept() {
            let req = read_full_http_request(&mut s);
            let body = if req.contains("torrent-add") {
                add_c.fetch_add(1, Ordering::SeqCst);
                r#"{"result":"success","arguments":{"torrent-added":{"hashString":"fedcba9876543210fedcba9876543210fedcba98"}}}"#
            } else if req.contains("torrent-get") {
                r#"{"result":"success","arguments":{"torrents":[]}}"#
            } else {
                r#"{"result":"success","arguments":{"rpc-version":17,"download-dir":"/downloads"}}"#
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (addr, add_calls)
}

#[tokio::test]
async fn admit_persists_patched_downloader_id_and_http_idempotency_key_replays() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let srv = StubTorrentServer::start(minimal_torrent_bytes("The.Matrix.1999.1080p", 4096));
    let dl = Arc::new(CountingSubmissionDl {
        calls: AtomicUsize::new(0),
    });
    let dl_b = DownloaderId::from_uuid(uuid::Uuid::from_u128(
        0xbbbb_bbbb_bbbb_4bbb_8bbb_bbbb_bbbb_bbbb,
    ));
    let title = format!("The.Matrix.1080p RetargetDl:{dl_b}");
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(StubSearchFetcher {
            payload: nexus_row(&title, "admit.torrent"),
        }),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr),
    );
    let env = HttpEnv {
        _dir: dir,
        state: state.clone(),
        app: router(state),
        member_token: String::new(),
    };
    let (site_id, sub_id) = seed_site_and_subscribe(&env, &srv.url("")).await;
    let (tr_addr, tr_adds) = spawn_transmission_add_stub();
    env.state
        .store()
        .insert_downloader(&api::store::DownloaderRow {
            id: dl_b,
            name: "secondary-tr".into(),
            kind: "transmission".into(),
            url: format!("http://{tr_addr}/transmission/rpc"),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: true,
        })
        .unwrap();
    enable_hooks_plugin(&env, "org.example.admit-retarget", &["download.before"]).await;

    let enc = srv.url("admit.torrent");
    let payload = json!({ "subscribe_id": sub_id.to_string(), "enclosure": enc });
    let hdr = [("idempotency-key", "k-admit-1")];
    let (st1, _, b1) = env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(payload.clone()),
        ))
        .await;
    assert_ok(st1, &b1, StatusCode::OK);
    let (st2, _, b2) = env
        .send(req_json(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            &hdr,
            Some(payload),
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::OK);
    assert_eq!(
        tr_adds.load(Ordering::SeqCst),
        1,
        "Idempotency-Key must replay admit"
    );
    let pending = env.state.store().load_pending(sub_id).unwrap();
    assert_eq!(pending[0].1.downloader_id, Some(dl_b));

    let sub_hdr = [("idempotency-key", "k-submit-1")];
    let sub_body = json!({ "download_url": srv.url("s.torrent"), "title": "Clean.1080p", "site_id": site_id.to_string() });
    let (ss1, _, bs1) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &sub_hdr,
            Some(sub_body.clone()),
        ))
        .await;
    assert_ok(ss1, &bs1, StatusCode::OK);
    let (ss2, _, bs2) = env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &sub_hdr,
            Some(sub_body),
        ))
        .await;
    assert_ok(ss2, &bs2, StatusCode::OK);
    assert_eq!(
        dl.calls.load(Ordering::SeqCst),
        1,
        "Idempotency-Key must replay manual submit"
    );
}

#[tokio::test]
async fn manual_submit_strips_nil_sentinel_and_preserves_retargeted_downloader_on_replay() {
    let ctx = setup_subscribe_env("The.Matrix.1999.1080p", "nil-check.torrent").await;
    let nil_body = json!({
        "download_url": ctx.srv.url("nil-check.torrent"),
        "title": "The.Matrix.1999.1080p",
        "site_id": ctx.site_id.to_string(),
        "subscribe_id": ctx.sub_id.to_string(),
        "category": "movies"
    });
    let (st_nil, _, b_nil) = ctx
        .env
        .send(req_json(
            "POST",
            "/api/v1/downloaders/submit",
            Some(TOKEN),
            &[],
            Some(nil_body),
        ))
        .await;
    assert_ok(st_nil, &b_nil, StatusCode::OK);
    let prefs_raw = ctx
        .env
        .state
        .store()
        .get_setting("download.target_prefs")
        .unwrap()
        .unwrap();
    let prefs: serde_json::Value = serde_json::from_str(&prefs_raw).unwrap();
    assert!(prefs["movies"]["downloader_id"].is_null(), "{prefs}");
    let pending = ctx.env.state.store().load_pending(ctx.sub_id).unwrap();
    assert_eq!(
        pending[0].1.downloader_id,
        Some(DownloaderId::from_uuid(uuid::Uuid::nil()))
    );

    let (dl_a, dl_b) = (DownloaderId::new(), DownloaderId::new());
    let (addr_a, _) = spawn_tr_add_stub("5555555555555555555555555555555555555555");
    let (addr_b, adds_b) = spawn_tr_add_stub("6666666666666666666666666666666666666666");
    for (id, name, addr, is_default) in [
        (dl_a, "def-a", addr_a, true),
        (dl_b, "sec-b", addr_b, false),
    ] {
        ctx.env
            .state
            .store()
            .insert_downloader(&api::store::DownloaderRow {
                id,
                name: name.into(),
                kind: "transmission".into(),
                url: format!("http://{addr}/transmission/rpc"),
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
    let run_uri = format!("/api/v1/subscriptions/{}/run", ctx.sub_id);
    let (st_run, _, b_run) = ctx
        .env
        .send(req_json("POST", &run_uri, Some(TOKEN), &[], None))
        .await;
    assert_ok(st_run, &b_run, StatusCode::OK);

    enable_hooks_plugin(
        &ctx.env,
        "org.example.replay-retarget",
        &["download.before"],
    )
    .await;
    let body = json!({
        "download_url": ctx.srv.url("replay-cat.torrent"),
        "title": format!("Clean.2160p RetargetDl:{dl_b}"),
        "site_id": ctx.site_id.to_string(),
        "category": "uhd"
    });
    for _ in 0..2 {
        let (st, _, b) = ctx
            .env
            .send(req_json(
                "POST",
                "/api/v1/downloaders/submit",
                Some(TOKEN),
                &[("idempotency-key", "idem-cat-retarget")],
                Some(body.clone()),
            ))
            .await;
        assert_ok(st, &b, StatusCode::OK);
    }
    assert_eq!(adds_b.load(Ordering::SeqCst), 1);
    let raw2 = ctx
        .env
        .state
        .store()
        .get_setting("download.target_prefs")
        .unwrap()
        .unwrap();
    let prefs_after: serde_json::Value = serde_json::from_str(&raw2).unwrap();
    assert_eq!(
        prefs_after["uhd"]["downloader_id"].as_str(),
        Some(dl_b.to_string().as_str())
    );
}
