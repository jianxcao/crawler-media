#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::http::StatusCode;
use domain::SiteId;
use parking_lot::Mutex;
use plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, assert_paged, req_json,
};
use serde_json::{Value, json};
use store::{
    ClaimSiteMaintenanceReceipt, SiteMaintenanceAction, SiteMaintenanceClaimResult,
    SiteMaintenanceReceiptFilter, SiteMaintenanceState,
};

struct StubServer {
    port: u16,
    stop: Arc<AtomicBool>,
    paths: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn start(
        handler: impl Fn(&str, usize) -> (u16, Vec<(&'static str, String)>, String) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let paths_c = paths.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = calls.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req_text = String::from_utf8_lossy(&buf[..n]);
                        let first_line = req_text.lines().next().unwrap_or("");
                        let path = first_line
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();
                        paths_c.lock().push(path.clone());
                        let call_idx = calls_c.fetch_add(1, Ordering::SeqCst);
                        let (code, extra_headers, body) = handler(&path, call_idx);
                        let reason = if code == 200 { "OK" } else { "ERR" };
                        let mut resp = format!(
                            "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
                            body.len()
                        );
                        for (k, v) in extra_headers {
                            resp.push_str(&format!("{k}: {v}\r\n"));
                        }
                        resp.push_str("\r\n");
                        resp.push_str(&body);
                        let _ = stream.write_all(resp.as_bytes());
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
            paths,
            calls,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct LocalLoopbackFetcher;

impl indexer::Fetcher for LocalLoopbackFetcher {
    fn fetch(&self, request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        let parsed = url::Url::parse(&request.url)
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let host = parsed.host_str().unwrap_or("127.0.0.1");
        let port = parsed.port_or_known_default().unwrap_or(80);
        let path = if let Some(q) = parsed.query() {
            format!("{}?{q}", parsed.path())
        } else {
            parsed.path().to_string()
        };
        let mut stream = std::net::TcpStream::connect((host, port))
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let mut buf = String::new();
        stream
            .read_to_string(&mut buf)
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        Ok(buf
            .split_once("\r\n\r\n")
            .map_or(buf.as_str(), |(_, b)| b)
            .to_string())
    }
}

async fn live_http_env() -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = api::Store::open(dir.path().join("data")).unwrap();
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage")));
    let fetcher = Arc::new(LocalLoopbackFetcher);
    let state = api::ApiState::new_arc_with_admin_password(
        Arc::new(store),
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        indexer::RegistrySnapshot::builtin().unwrap(),
        fetcher,
        dl,
    )
    .unwrap();
    let app = api::router(state.clone());
    HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    }
}

async fn create_site_with_tz(env: &HttpEnv, url: &str, tz: &str) -> Value {
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "NexusTest",
                "url": url,
                "profile_id": "pterclub",
                "cookie": "uid=1; pass=initial",
                "maintenance_via": "http",
                "timezone": tz,
                "enabled": true,
            })),
        ))
        .await;
    assert_ok(st, &body, StatusCode::CREATED)
}

#[tokio::test]
async fn timezone_validation_and_dst_window_bounds_work_offline() {
    let env = HttpEnv::new().await;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "BadTz",
                "url": "https://pt.example/",
                "profile_id": "pterclub",
                "cookie": "uid=1",
                "timezone": "Mars/Olympus_Mons",
                "enabled": true,
            })),
        ))
        .await;
    assert_err(st, &body, StatusCode::BAD_REQUEST, "site.invalid_timezone");

    let created = create_site_with_tz(&env, "https://pt.example/", "Asia/Shanghai").await;
    let site_id = created["id"].as_str().unwrap();
    let (st, _, body) = env
        .send(req_json(
            "PATCH",
            &format!("/api/v1/sites/{site_id}"),
            Some(TOKEN),
            &[],
            Some(json!({ "timezone": "Not/A_Zone" })),
        ))
        .await;
    assert_err(st, &body, StatusCode::BAD_REQUEST, "site.invalid_timezone");

    // Asia/Shanghai (UTC+8): 2026-11-21 16:00:00Z is 2026-11-22 00:00:00+08:00
    let shanghai = api::maintenance_window::compute_window("Asia/Shanghai", 1_795_276_800).unwrap();
    assert_eq!(shanghai.window_key, "2026-11-22");
    assert_eq!(shanghai.window_start_utc, 1_795_276_800);
    assert_eq!(shanghai.window_end_utc - shanghai.window_start_utc, 86_400);

    // America/New_York spring forward 2026-03-08: 23h day (05:00Z .. +23h)
    let ny_spring =
        api::maintenance_window::compute_window("America/New_York", 1_772_964_000).unwrap();
    assert_eq!(ny_spring.window_key, "2026-03-08");
    assert_eq!(
        ny_spring.window_end_utc - ny_spring.window_start_utc,
        82_800
    );

    // America/New_York fall back 2026-11-01: 25h day
    let ny_fall =
        api::maintenance_window::compute_window("America/New_York", 1_793_530_800).unwrap();
    assert_eq!(ny_fall.window_key, "2026-11-01");
    assert_eq!(ny_fall.window_end_utc - ny_fall.window_start_utc, 90_000);
}

async fn install_cm_checkin_task_plugin(env: &HttpEnv) -> String {
    let pid = "cm.checkin";
    env.enable_dev_mode().await;
    let manifest = "manifest_version = 1\nid = \"cm.checkin\"\nname = \"Checkin\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"Test\"]\ndescription = \"Checkin task\"\nconfig_schema = \"schemas/config.schema.json\"\nstate_schema = 1\n[[components]]\nid = \"worker\"\nentry = \"components/worker.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\npermissions = [\"kv\"]\n[[components.tasks]]\nid = \"check-in\"\ninput_schema = \"schemas/task.schema.json\"\n[components.tasks.schedule]\nkind = \"interval\"\nseconds = 86400\n";
    let cfg_schema = plugin_http_support::pkg::sample_schema(false, None);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"action":{"type":"string"}}}"#;
    let pkg = plugin_http_support::pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        (
            "components/worker.wasm",
            plugin_http_support::pkg::task_wasm(),
        ),
    ]);
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let inst = env.install_pkg(&pkg, None, grants).await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin(pid, rev, "en-cm-checkin-task").await;
    pid.to_string()
}

#[tokio::test]
async fn builtin_plugins_bootstrap_idempotently_and_accept_site_maintenance_command() {
    let env = HttpEnv::new().await;
    let (st_gone, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.checkin",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_gone, StatusCode::NOT_FOUND);

    let (st, _, body) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.nexusphp-maintenance",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let maint = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(maint["id"], "builtin.nexusphp-maintenance");
    assert_eq!(maint["lifecycle"], "enabled");
    assert_eq!(maint["source"], "builtin");

    let checkin_id = install_cm_checkin_task_plugin(&env).await;
    let (st, _, body) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{checkin_id}/tasks"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (items, _) = assert_paged(st, &body);
    let t = items.iter().find(|i| i["id"] == "check-in").unwrap();
    assert_eq!(t["schedule"]["interval_secs"], 86400);

    let rev = maint["revision"].as_i64().unwrap();
    let etag = format!("\"{rev}\"");
    let srv = StubServer::start(|path, _| {
        if path.contains("attendance.php") {
            (
                200,
                vec![],
                "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>".into(),
            )
        } else {
            (404, vec![], "missing".into())
        }
    });
    let site = create_site_with_tz(&env, &srv.url(), "UTC").await;
    let site_id = site["id"].as_str().unwrap();

    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.nexusphp-maintenance/commands",
            Some(TOKEN),
            &[
                ("if-match", &etag),
                ("idempotency-key", "builtin-maint-cmd-1"),
            ],
            Some(json!({
                "kind": "site.maintenance",
                "payload": {
                    "site_id": site_id,
                    "action": "check-in",
                },
                "targets": [],
            })),
        ))
        .await;
    let op = assert_ok(st, &body, StatusCode::ACCEPTED);
    let op_id = op["operation_id"].as_str().unwrap().to_string();

    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/jobs/tick?now=1795276800",
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK);

    let (st, _, body) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{op_id}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let op_view = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(op_view["state"], "succeeded");
    assert_eq!(srv.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn verify_uses_verify_session_without_space_search_and_deduplicates_per_window() {
    let env = live_http_env().await;
    let srv = StubServer::start(|_path, _| {
        (
            200,
            vec![],
            "<html><body><a href=\"logout.php\">Logout</a></body></html>".into(),
        )
    });
    let site = create_site_with_tz(&env, &srv.url(), "UTC").await;
    let site_id = site["id"].as_str().unwrap();

    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/verify"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let v1 = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(v1["ok"], true);
    assert_eq!(srv.calls.load(Ordering::SeqCst), 1);
    let paths = srv.paths.lock().clone();
    assert!(
        paths
            .iter()
            .all(|p| !p.contains("search=%20") && !p.contains("search=+")),
        "verify must never issue space search: {paths:?}"
    );

    // Second verify in same local-day window must short-circuit with 0 HTTP calls
    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/verify"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let v2 = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(v2["ok"], true);
    assert_eq!(srv.calls.load(Ordering::SeqCst), 1);

    let parsed_id: SiteId = site_id.parse().unwrap();
    let page = env
        .state
        .store()
        .list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter {
                site_id: Some(parsed_id),
                action: Some(SiteMaintenanceAction::VerifySession),
                state: None,
            },
            None,
            Some(10),
        )
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].state, SiteMaintenanceState::Completed);
}

async fn assert_unknown_receipt_freezes_checkin(env: &HttpEnv, srv: &StubServer) {
    let site2 = create_site_with_tz(env, &srv.url(), "UTC").await;
    let s2_id_str = site2["id"].as_str().unwrap();
    let s2_id: SiteId = s2_id_str.parse().unwrap();
    let obs2 = env.state.store().observe_site(s2_id).unwrap().unwrap();
    let w = api::maintenance_window::compute_window("UTC", 1_795_276_800).unwrap();
    let claimed = env
        .state
        .store()
        .claim_site_maintenance_receipt(&ClaimSiteMaintenanceReceipt {
            site_id: s2_id,
            action: SiteMaintenanceAction::CheckIn,
            window_key: w.window_key,
            timezone: "UTC".into(),
            window_start_utc: i64::MIN / 2,
            window_end_utc: i64::MAX / 2,
            now_utc: 1_795_276_800,
            plugin_id: "builtin.nexusphp-maintenance".into(),
            provider_id: "plugin:builtin.nexusphp-maintenance/default".into(),
            generation: 1,
            credential_epoch: obs2.credential_epoch().storage_key().into(),
        })
        .unwrap();
    let SiteMaintenanceClaimResult::Claimed(receipt) = claimed else {
        panic!("unexpected claim");
    };
    env.state
        .store()
        .mark_site_maintenance_receipt_unknown(
            &receipt.receipt_id,
            receipt.revision,
            None,
            None,
            1_795_276_801,
        )
        .unwrap();

    let _ = env
        .send(req_json(
            "PATCH",
            &format!("/api/v1/sites/{s2_id_str}"),
            Some(TOKEN),
            &[],
            Some(json!({ "cookie": "uid=1; pass=manual-new", "timezone": "Asia/Shanghai" })),
        ))
        .await;
    let calls_before = srv.calls.load(Ordering::SeqCst);
    let (st, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{s2_id_str}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(srv.calls.load(Ordering::SeqCst), calls_before);
}

fn assert_boost_check_deleted(env: &HttpEnv, site_id_str: &str) {
    env.state
        .store()
        .put_setting(
            &format!("boost.{site_id_str}"),
            &json!({ "enabled": true, "budget_bytes": 1 }).to_string(),
        )
        .unwrap();
    let _ = api::check_in_run_for_test(&env.state);
    let boost_raw = env
        .state
        .store()
        .get_setting(&format!("boost.{site_id_str}"))
        .unwrap()
        .unwrap();
    let boost_json: Value = serde_json::from_str(&boost_raw).unwrap();
    assert!(
        boost_json.get("upper_limit_reached").is_none(),
        "boost_check must be deleted: {boost_json}"
    );
}

#[tokio::test]
async fn receipts_freeze_on_unknown_allow_retry_on_challenge_and_preserve_manual_cookie_cas() {
    let env = HttpEnv::new().await;
    let srv = StubServer::start(|path, call| {
        if path.contains("attendance.php") {
            if call == 0 {
                (
                    200,
                    vec![],
                    "<html><body><div>请完成安全验证 captcha</div></body></html>".into(),
                )
            } else {
                (
                    200,
                    vec![("Set-Cookie", "pass=server-rotated; Path=/".into())],
                    "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>"
                        .into(),
                )
            }
        } else {
            (
                200,
                vec![("Set-Cookie", "pass=login-rotated; Path=/".into())],
                "<html><body><a href=\"logout.php\">Logout</a></body></html>".into(),
            )
        }
    });
    let site = create_site_with_tz(&env, &srv.url(), "UTC").await;
    let site_id_str = site["id"].as_str().unwrap();
    let site_id: SiteId = site_id_str.parse().unwrap();

    let (st, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id_str}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id_str}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK);

    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id_str}/login"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK);
    let updated_site = env.state.store().get_site(site_id).unwrap().unwrap();
    assert!(
        updated_site
            .cookie
            .as_deref()
            .unwrap()
            .contains("login-rotated")
    );

    assert_unknown_receipt_freezes_checkin(&env, &srv).await;
    assert_boost_check_deleted(&env, site_id_str);
}

#[tokio::test]
async fn site_plugin_status_redacts_secrets_and_plugin_actions_accepts_202() {
    let env = HttpEnv::new().await;
    let srv = StubServer::start(|_path, _| {
        (
            200,
            vec![],
            "<html><body><a href=\"logout.php\">Logout</a></body></html>".into(),
        )
    });
    let created = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "StatusSite",
                "url": srv.url(),
                "profile_id": "pterclub",
                "cookie": "uid=1; pass=top-secret-cookie",
                "cdp_url": "http://127.0.0.1:9222/secret-cdp",
                "maintenance_via": "http",
                "timezone": "UTC",
                "enabled": true,
            })),
        ))
        .await;
    let site = assert_ok(created.0, &created.2, StatusCode::CREATED);
    let site_id = site["id"].as_str().unwrap();

    let (st, headers, body) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/sites/{site_id}/plugin-status"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let status = assert_ok(st, &body, StatusCode::OK);
    let raw = status.to_string();
    assert!(!raw.contains("top-secret-cookie"));
    assert!(!raw.contains("secret-cdp"));
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();

    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/plugin-actions"),
            Some(TOKEN),
            &[("if-match", &etag), ("idempotency-key", "site-act-1")],
            Some(json!({
                "action": "verify",
                "provider_id": "plugin:builtin.nexusphp-maintenance/default"
            })),
        ))
        .await;
    let op = assert_ok(st, &body, StatusCode::ACCEPTED);
    assert!(op["operation_id"].is_string());
}

struct ApiMaintStub {
    desc: indexer::MaintenanceDescriptor,
    calls: Arc<AtomicUsize>,
}

impl indexer::MaintenanceProvider for ApiMaintStub {
    fn descriptor(&self) -> &indexer::MaintenanceDescriptor {
        &self.desc
    }

    fn run(
        &self,
        _ctx: &indexer::MaintenanceCallContext<'_>,
        _site: &domain::Site,
        _action: indexer::MaintenanceAction,
        _job_id: &str,
    ) -> Result<indexer::MaintenanceOutcome, indexer::IndexerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(indexer::MaintenanceOutcome {
            state: indexer::MaintenanceState::Completed,
            evidence: "api-plugin-ok".into(),
            session_capture: None,
        })
    }
}

#[tokio::test]
async fn framework_api_site_with_maintenance_plugin_is_not_skipped_and_cas_race_keeps_receipt() {
    let env = HttpEnv::new().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let api_yaml = r#"
id: custom-api
framework: api
base_url: https://custom-api.example
search:
  path: /api/search
  query_param: keyword
"#;
    let mut candidate = indexer::RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.apimaint", "1.0.0", 1, api_yaml)
        .unwrap();
    candidate = candidate
        .add_maintenance_provider(Arc::new(ApiMaintStub {
            desc: indexer::MaintenanceDescriptor {
                id: "plugin:org.example.apimaint/maint".into(),
                source: indexer::ProviderSource::PluginWasm {
                    plugin_id: "org.example.apimaint".into(),
                    component_id: "maint".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                actions: vec![indexer::MaintenanceAction::CheckIn],
                availability: indexer::ProviderAvailability::Enabled,
            },
            calls: calls.clone(),
        }))
        .unwrap();
    env.state.indexer().publish(candidate).unwrap();

    // 1) Framework::Api site WITH maintenance plugin -> executed by check_in::run
    let api_site_id = SiteId::new();
    env.state
        .store()
        .insert_site(&domain::Site {
            id: api_site_id,
            name: "ApiPluginSite".into(),
            url: "https://custom-api.example/".into(),
            profile_id: "plugin:org.example.apimaint/custom-api".into(),
            cookie: Some("uid=1".into()),
            api_key: Some("k".into()),
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
    // 2) Builtin Framework::Api site (mteam) WITHOUT maintenance provider -> gracefully skipped
    let mteam_id = SiteId::new();
    env.state
        .store()
        .insert_site(&domain::Site {
            id: mteam_id,
            name: "MTeam".into(),
            url: "https://api.m-team.cc/".into(),
            profile_id: "mteam".into(),
            cookie: None,
            api_key: Some("k".into()),
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

    api::check_in_run_for_test(&env.state).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Framework::Api site with enabled maintenance provider must not be skipped"
    );

    // Verify CAS cookie race preservation on NexusPHP site: if cookie changes mid-flight,
    // save_site_if_current preserves manual cookie while keeping Completed receipt.
    let srv = StubServer::start(|_, _| {
        (
            200,
            vec![("Set-Cookie", "pass=stale-rotated; Path=/".into())],
            "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>".into(),
        )
    });
    let race_site = create_site_with_tz(&env, &srv.url(), "UTC").await;
    let race_id: SiteId = race_site["id"].as_str().unwrap().parse().unwrap();
    let old_obs = env.state.store().observe_site(race_id).unwrap().unwrap();
    let mut manual_site = old_obs.site().clone();
    manual_site.cookie = Some("uid=1; pass=manual-fresh".into());
    env.state
        .store()
        .save_site(&manual_site, store::ManualCredentialWrite::Replace)
        .unwrap();
    let saved = env
        .state
        .store()
        .save_site_if_current(&old_obs, Some("uid=1; pass=stale-rotated"))
        .unwrap();
    assert!(!saved, "CAS conflict must not overwrite manual credential");
    assert_eq!(
        env.state
            .store()
            .get_site(race_id)
            .unwrap()
            .unwrap()
            .cookie
            .as_deref(),
        Some("uid=1; pass=manual-fresh")
    );
}
