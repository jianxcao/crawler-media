#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::{MaintenanceVia, Site, SiteId};
use indexer::{
    Browser, BrowserConfig, PluginBrowserError, PluginBrowserErrorKind, PluginBrowserPage,
    PluginNavResponse,
};
use plugin_api::{BrowserPurpose, ErrorCode, ErrorDetail, GuestRole, PluginTrust};
use plugin_host::Invocation;
use plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok,
    pkg::{TestSigningKey, build_stored_zip},
    req_json,
};
use serde_json::{Value, json};

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");

struct FakeBrowserPageState {
    closed: AtomicBool,
    fail_close: AtomicBool,
    filled_values: parking_lot::Mutex<Vec<String>>,
}

struct FakePluginPage {
    st: Arc<FakeBrowserPageState>,
}

impl PluginBrowserPage for FakePluginPage {
    fn set_cookies(&mut self, _page_url: &str, _cookie: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }

    fn navigate(
        &mut self,
        _url: &str,
        _wait_until: &str,
    ) -> Result<PluginNavResponse, PluginBrowserError> {
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("Set-Cookie".into(), "secret=1".into()), ("Content-Type".into(), "text/html".into())],
            body: b"<html><body><script>evil()</script><input type=\"password\" value=\"hunter2\"><a class=\"row\" href=\"/t/1\">Result One</a><div class=\"done\">ok</div></body></html>".to_vec(),
        })
    }

    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_origin") {
            return Ok(json!("https://pt.example"));
        }
        if expr.contains("__cm_query") {
            Ok(json!([{"visible_text": "Result One", "tag": "a"}]))
        } else if expr.contains("__cm_attr") {
            Ok(json!("/t/1"))
        } else if expr.contains("__cm_text") {
            Ok(json!("Result One"))
        } else if expr.contains("__cm_fill") {
            self.st.filled_values.lock().push(expr.to_string());
            Ok(json!(true))
        } else if expr.contains("__cm_dom") {
            Ok(json!(
                "<html><body><script>alert(1)</script><input data-value=\"foo\" type=\"password\" value=\"hunter2\"><input type=password value=secret2><div class=\"row\">Result One</div></body></html>"
            ))
        } else {
            Ok(json!(true))
        }
    }

    fn cookie_header(&mut self, _page_url: &str) -> Result<String, PluginBrowserError> {
        Ok("fresh=cookie123".into())
    }

    fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        self.st.closed.store(true, Ordering::SeqCst);
        if self.st.fail_close.load(Ordering::SeqCst) {
            return Err(PluginBrowserError::new(
                PluginBrowserErrorKind::Transport,
                "dispose failed",
            ));
        }
        Ok(())
    }
}

fn build_signed_indexer_pkg(plugin_id: &str, key: &TestSigningKey) -> (Vec<u8>, String) {
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Browser Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Browser test"
config_schema = "config.schema.json"
state_schema = 1
credential_binding = "invocation-site"

[[components]]
id = "fixture"
entry = "indexer.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["http", "http:site-auth", "read:sites", "browser:search", "indexer:provide"]

[[components.indexers]]
id = "fixture"
name = "Fixture"
kind = "pt"
capabilities = ["search", "rss", "detail", "resolve-download", "user-stats", "categories"]
site_schema = "site.schema.json"
"#
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    let pkg = build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("site.schema.json", schema.as_bytes()),
        ("indexer.wasm", INDEXER_WASM),
    ]);
    let sig = key.sign_package(&pkg);
    (pkg, sig)
}

fn insert_browser_site(store: &store::Store, profile_id: &str) -> SiteId {
    let site_id = SiteId::new();
    let site = Site {
        id: site_id,
        name: "BrowserSite".into(),
        url: "https://pt.example".into(),
        profile_id: profile_id.into(),
        cookie: Some("c=secret".into()),
        api_key: Some("k=secret".into()),
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: true,
        timezone: "UTC".into(),
    };
    store.insert_site(&site).unwrap();
    store.put_setting("cdp.enabled", "true").unwrap();
    site_id
}

fn make_fake_browser() -> (Arc<Browser>, Arc<FakeBrowserPageState>) {
    let page_state = Arc::new(FakeBrowserPageState {
        closed: AtomicBool::new(false),
        fail_close: AtomicBool::new(false),
        filled_values: parking_lot::Mutex::new(Vec::new()),
    });
    let opener_state = page_state.clone();
    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(200))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(move |_| {
            Ok(Box::new(FakePluginPage {
                st: opener_state.clone(),
            }))
        }),
    );
    (browser, page_state)
}

#[tokio::test]
async fn third_party_signed_plugin_gets_browser_isolation_unavailable_while_allowlisted_succeeds() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(9);
    env.pin_trust_key(&key, "test").await;

    let (pkg, sig) = build_signed_indexer_pkg("org.example.signed-browser", &key);
    let site_id = insert_browser_site(
        env.state.store().as_ref(),
        "plugin:org.example.signed-browser/fixture",
    );
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "browser:search", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": site_id.to_string()}, "sensitive": true}
    ]);
    let installed = env.install_pkg(&pkg, Some(&sig), grants).await;
    let op = env
        .enable_plugin(
            "org.example.signed-browser",
            installed["revision"].as_i64().unwrap(),
            "en-signed-b",
        )
        .await;
    assert_eq!(op["state"], "succeeded");

    let (browser, page_state) = make_fake_browser();
    let state_untrusted = env.state.clone().with_browser(browser);
    let ports = state_untrusted.plugins().host_ports();
    let epoch = ports
        .site_credential_epoch(&site_id.to_string())
        .unwrap()
        .unwrap();
    let mut inv = Invocation::new("org.example.signed-browser", Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(site_id.to_string());
    inv.credential_epoch = Some(epoch.clone());
    inv.trust = PluginTrust::Untrusted;
    let _g = ports.begin_invocation(&inv).unwrap();
    let err = ports
        .browser_open(&inv, BrowserPurpose::Search)
        .err()
        .unwrap();
    assert_eq!(err.code, ErrorCode::Unsupported);
    assert_eq!(err.detail, Some(ErrorDetail::BrowserIsolationUnavailable));
    drop(_g);

    let state_trusted = state_untrusted
        .clone()
        .with_admin_trusted_plugins(["org.example.signed-browser"]);
    let ports_t = state_trusted.plugins().host_ports();
    inv.trust = PluginTrust::AdminTrusted;
    let _g2 = ports_t.begin_invocation(&inv).unwrap();
    let mut sess = ports_t.browser_open(&inv, BrowserPurpose::Search).unwrap();
    let receipt = sess.navigate("/search", "load").unwrap();
    let dom = sess.dom(4096).unwrap();
    assert!(!dom.contains("<script>"));
    assert!(!dom.contains("hunter2"));
    assert!(!dom.contains("secret2"));
    let resp = sess.read_response(&receipt, 4096).unwrap();
    assert!(
        resp.headers
            .iter()
            .all(|h| !h.name.eq_ignore_ascii_case("set-cookie"))
    );
    sess.close().unwrap();
    assert!(page_state.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn sites_reject_invalid_timezone_and_browser_proxy_and_count_managed_sites() {
    let env = HttpEnv::new().await;
    let (st_tz, _, b_tz) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "BadTz",
                "url": "https://bad.example",
                "profile_id": "demo",
                "timezone": "   "
            })),
        ))
        .await;
    assert_err(
        st_tz,
        &b_tz,
        StatusCode::BAD_REQUEST,
        "site.invalid_timezone",
    );

    let (st_px, _, b_px) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "BadProxy",
                "url": "https://bad.example",
                "profile_id": "demo",
                "browser_search": true,
                "proxy": "http://127.0.0.1:7890"
            })),
        ))
        .await;
    assert_err(
        st_px,
        &b_px,
        StatusCode::BAD_REQUEST,
        "site.browser_proxy_unsupported",
    );

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "SearchBrowserSite",
                "url": "https://good.example",
                "profile_id": "demo",
                "browser_search": true,
                "timezone": "UTC"
            })),
        ))
        .await;
    assert_ok(st_ok, &b_ok, StatusCode::CREATED);

    let (st_bs, _, b_bs) = env
        .send(req_json(
            "GET",
            "/api/v1/settings/browser",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs = assert_ok(st_bs, &b_bs, StatusCode::OK);
    assert_eq!(bs["managed_browser_sites"], 1);
}

#[tokio::test]
async fn browser_availability_and_checks_routes_enforce_contracts() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "SearchBrowserSite",
                "url": "https://good.example",
                "profile_id": "demo",
                "browser_search": true,
                "timezone": "UTC"
            })),
        ))
        .await;
    let created = assert_ok(st_ok, &b_ok, StatusCode::CREATED);
    let site_id = created["id"].as_str().unwrap().to_string();

    let key = TestSigningKey::generate(11);
    let (pkg, _) = build_signed_indexer_pkg("org.example.avail", &key);
    let installed = env.install_pkg(&pkg, None, json!([])).await;
    let op_en = env
        .enable_plugin(
            "org.example.avail",
            installed["revision"].as_i64().unwrap(),
            "en-avail",
        )
        .await;
    assert_eq!(op_en["state"], "succeeded");

    let (st_p, _, b_p) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.avail",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let detail = assert_ok(st_p, &b_p, StatusCode::OK);
    let if_match = format!("\"{}\"", detail["revision"].as_i64().unwrap());

    let (st_av, _, b_av) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/org.example.avail/browser-availability?site_id={site_id}&purpose=search"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let av = assert_ok(st_av, &b_av, StatusCode::OK);
    assert_eq!(av["available"], false);
    assert!(!b_av.to_string().contains("9222"));

    let (st_chk, _, b_chk) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.avail/browser-checks",
            Some(TOKEN),
            &[("idempotency-key", "chk-1"), ("if-match", &if_match)],
            Some(json!({"site_id": site_id, "purpose": "search"})),
        ))
        .await;
    let op = assert_ok(st_chk, &b_chk, StatusCode::ACCEPTED);
    assert!(op["operation_id"].is_string());
}

#[tokio::test]
async fn builtin_checkin_accepts_site_scoped_browser_maintenance_grant() {
    let env = HttpEnv::new().await;
    let (st_site, _, b_site) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "CheckinBrowserSite",
                "url": "https://good.example",
                "profile_id": "demo",
                "maintenance_via": "browser",
                "timezone": "UTC"
            })),
        ))
        .await;
    let site_id = assert_ok(st_site, &b_site, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st_av0, _, b_av0) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/builtin.checkin/browser-availability?site_id={site_id}&purpose=maintenance"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let av0 = assert_ok(st_av0, &b_av0, StatusCode::OK);
    assert_eq!(av0["grant_present"], false);

    let grants = json!([{
        "capability": "browser:maintenance",
        "scope": { "Site": site_id },
        "sensitive": true
    }]);
    let rev = env.get_plugin_rev("builtin.checkin").await;
    env.disable_plugin("builtin.checkin", rev, "dis-bm").await;
    let rev_dis = env.get_plugin_rev("builtin.checkin").await;
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.checkin/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grants })),
        ))
        .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::OK);
    let (st_gr, _, b_gr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.checkin/permissions/grant",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", "checkin-bm-1"),
            ],
            Some(json!({
                "consent_token": prep["consent_token"],
                "grants": grants
            })),
        ))
        .await;
    assert_eq!(
        assert_ok(st_gr, &b_gr, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
    let rev_gr = env.get_plugin_rev("builtin.checkin").await;
    env.enable_plugin("builtin.checkin", rev_gr, "en-bm").await;

    let (st_av1, _, b_av1) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/builtin.checkin/browser-availability?site_id={site_id}&purpose=maintenance"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let av1 = assert_ok(st_av1, &b_av1, StatusCode::OK);
    assert_eq!(av1["grant_present"], true);
}

fn insert_local_site(store: &api::Store, name: &str, addr: std::net::SocketAddr) -> SiteId {
    let id = SiteId::new();
    store
        .insert_site(&Site {
            id,
            name: name.into(),
            url: format!("http://{addr}/"),
            profile_id: "demo".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

#[tokio::test]
async fn provider_site_grants_enforced_and_set_cookie_capture_restricted_to_maintenance() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().take(8).flatten() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nSet-Cookie: sid=fresh1; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
        }
    });

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let state = plugin_http_support::build_state(dir.path()).with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(addr),
    );
    let env = HttpEnv {
        _dir: dir,
        app: api::router(state.clone()),
        state,
        member_token: String::new(),
    };
    env.enable_dev_mode().await;
    let st = env.state.store();
    let (site_a, site_b) = (
        insert_local_site(&st, "ProvA", addr),
        insert_local_site(&st, "ProvB", addr),
    );

    const MAINT_WASM: &[u8] =
        include_bytes!("../../plugin-host/tests/fixtures/maintenance_fixture.wasm");
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    let manifest = "manifest_version = 1\nid = \"org.example.prov\"\nname = \"Prov\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\ncredential_binding = \"invocation-site\"\n[[components]]\nid = \"idx\"\nentry = \"indexer.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"indexer:provide\"]\n[[components.indexers]]\nid = \"fixture\"\nname = \"Idx\"\nkind = \"pt\"\ncapabilities = [\"search\", \"rss\", \"detail\", \"resolve-download\", \"user-stats\", \"categories\"]\nsite_schema = \"site.schema.json\"\n[[components]]\nid = \"maint\"\nentry = \"maint.wasm\"\nworld = \"crawler:plugin/maintenance-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"maintenance:provide\"]\n[[components.maintenance]]\nid = \"maint\"\nactions = [\"verify-session\", \"check-in\"]\n";
    let zip = build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("site.schema.json", schema.as_bytes()),
        ("indexer.wasm", INDEXER_WASM),
        ("maint.wasm", MAINT_WASM),
    ]);
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": site_a.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": site_a.to_string()}, "sensitive": true},
        {"capability": "maintenance:provide", "scope": {"Site": site_a.to_string()}, "sensitive": true}
    ]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(
        "org.example.prov",
        inst["revision"].as_i64().unwrap(),
        "en-prov",
    )
    .await;

    let ports = env.state.plugins().host_ports();
    let ep_a = ports
        .site_credential_epoch(&site_a.to_string())
        .unwrap()
        .unwrap();
    let ep_b = ports
        .site_credential_epoch(&site_b.to_string())
        .unwrap()
        .unwrap();
    for role in [GuestRole::Indexer, GuestRole::Maintenance] {
        let mut bad = Invocation::new("org.example.prov", Duration::from_secs(5));
        bad.generation = 1;
        bad.role = role;
        bad.site_id = Some(site_b.to_string());
        bad.credential_epoch = Some(ep_b.clone());
        assert_eq!(
            ports.begin_invocation(&bad).err().unwrap().code,
            ErrorCode::PermissionDenied
        );
    }

    let fetch_req = || plugin_api::HttpRequest {
        method: "GET".into(),
        target: plugin_api::HttpTarget::SiteRelative(plugin_api::SiteTarget {
            path: "/ping".into(),
            query: vec![],
            auth_slot: Some("cookie".into()),
        }),
        headers: vec![],
        body: None,
        max_bytes: 4096,
    };
    for (role, expect_receipt) in [(GuestRole::Indexer, false), (GuestRole::Maintenance, true)] {
        let mut inv = Invocation::new("org.example.prov", Duration::from_secs(5));
        inv.generation = 1;
        inv.role = role;
        inv.site_id = Some(site_a.to_string());
        inv.credential_epoch = Some(ep_a.clone());
        let g = ports.begin_invocation(&inv).unwrap();
        let has_receipt = ports
            .http_fetch(&inv, fetch_req())
            .unwrap()
            .receipt
            .is_some();
        assert_eq!(has_receipt, expect_receipt);
        drop(g);
    }
}

#[tokio::test]
async fn run_task_job_resolves_site_id_from_input_override_with_config_fallback() {
    struct CaptureRunner(Arc<parking_lot::Mutex<Vec<Option<SiteId>>>>);
    impl api::plugins::PluginTaskRunner for CaptureRunner {
        fn run_task(
            &self,
            _state: &api::ApiState,
            inv: &api::plugins::PluginTaskInvocation,
        ) -> Result<(), String> {
            self.0.lock().push(inv.authority.site_id());
            Ok(())
        }
    }

    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let state = plugin_http_support::build_state(dir.path())
        .with_plugin_task_runner(Arc::new(CaptureRunner(seen.clone())));
    let env = HttpEnv {
        _dir: dir,
        app: api::router(state.clone()),
        state,
        member_token: String::new(),
    };
    env.enable_dev_mode().await;
    let (site_a, site_b) = (env.insert_site("TaskSiteA"), env.insert_site("TaskSiteB"));

    let pid = "org.example.cb-task-input";
    let zip = plugin_http_support::pkg::build_task_package_custom(
        pid,
        "1.0.0",
        1,
        false,
        None,
        true,
        &["http:site-auth"],
    );
    let inst = env
        .install_pkg(
            &zip,
            None,
            json!([{"capability": "http:site-auth", "scope": "Global", "sensitive": true}]),
        )
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-cb-task")
        .await;

    let binding = env
        .state
        .store()
        .get_plugin_task_binding(pid, "sync")
        .unwrap()
        .unwrap();
    let rec = env.state.store().get_plugin(pid).unwrap().unwrap();
    let def = store::plugins::PluginTaskDefinition {
        enabled: true,
        interval_secs: Some(300),
        public_config: json!({ "site_id": site_b.to_string() }),
    };
    env.state
        .store()
        .save_plugin_task_binding(&rec, "sync", binding.definition_revision, &def)
        .unwrap();

    let b_id = domain::PluginTaskBindingId::from_uuid(binding.binding_id);
    let mk_job = |input: Value| jobs::Job {
        id: domain::JobId::new(),
        def_id: None,
        kind: jobs::JobKind::PluginTask,
        scope: jobs::JobScope::PluginTask(b_id),
        status: jobs::JobStatus::Running,
        attempt: 1,
        payload: json!({ "binding_id": b_id, "plugin_id": pid, "task_id": "sync", "input": input })
            .to_string(),
        run_after: 0,
        started_at: Some(1),
        finished_at: None,
        error: None,
        progress: None,
        concurrency_key: None,
        timeout_secs: Some(60),
    };
    api::plugins::run_task_job(
        &env.state,
        &mk_job(json!({ "site_id": site_a.to_string() })),
    )
    .unwrap();
    api::plugins::run_task_job(&env.state, &mk_job(json!({ "extra": 1 }))).unwrap();
    assert_eq!(*seen.lock(), vec![Some(site_a), Some(site_b)]);
}
