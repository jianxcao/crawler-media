#[path = "builtin_boost/support.rs"]
pub(crate) mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, MaintenanceVia, Site, SiteId};
use downloader::{Downloader, MemoryDownloader};
use indexer::{
    Browser, BrowserConfig, PluginBrowserError, PluginBrowserPage, PluginNavResponse,
    RegistrySnapshot,
};
use plugin_api::{
    BrowserFillValue, BrowserPurpose, ErrorCode, ErrorDetail, GuestRole, PluginTrust,
};
use plugin_host::Invocation;
use serde_json::{Value, json};
use support::NoopFetcher;
use support::plugin_http_support::{
    HttpEnv, TOKEN, assert_ok,
    pkg::{TestSigningKey, build_stored_zip},
    req_json,
};

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");

fn make_boost_r9_env() -> (HttpEnv, DownloaderId) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl_root = dir.path().join("dl_default");
    std::fs::create_dir_all(&dl_root).unwrap();

    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let env_dl = Arc::new(MemoryDownloader::new(&dl_root));
    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        env_dl as Arc<dyn Downloader>,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };

    let disabled_dl = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: disabled_dl,
            name: "Disabled-qB".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: Some("admin".into()),
            password: Some("pw".into()),
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: false,
        })
        .unwrap();
    (env, disabled_dl)
}

#[tokio::test]
async fn boost_disabled_downloader_returns_unavailable_disk_and_skips_submission() {
    let (env, disabled_dl) = make_boost_r9_env();

    // 1. Explicit disabled `downloader_id` in `brush-state` must NOT fall back to `default_dl`
    let (st_bs, _, b_bs) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/builtin.boost/brush-state?downloader_id={disabled_dl}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs = assert_ok(st_bs, &b_bs, StatusCode::OK);
    assert_eq!(
        bs["disk"]["status"], "unavailable",
        "disabled downloader_id must report unavailable disk, not default_dl free space: {bs}"
    );
    assert!(bs["disk"]["available_bytes"].is_null());

    // 2. Configuring `builtin.boost` with disabled `downloader_id` must reject/skip boost cycle
    let site_id = env.insert_site("R9-Boost-Site");
    let (_, _, cfg_get) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = cfg_get["data"]["schema_revision"].as_str().unwrap();
    let rev_pre = env.get_plugin_rev("builtin.boost").await;
    let (st_dis, _, b_dis) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/disable",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_pre}\"")),
                ("idempotency-key", "r9-boost-dis"),
            ],
            Some(json!({})),
        ))
        .await;
    assert_ok(st_dis, &b_dis, StatusCode::ACCEPTED);

    let rev0 = env.get_plugin_rev("builtin.boost").await;
    let (st_cfg, _, b_cfg) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/builtin.boost/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev0}\"")),
                ("idempotency-key", "r9-boost-cfg"),
            ],
            Some(json!({
                "schema_revision": srev,
                "values": {
                    "downloader_id": disabled_dl.to_string(),
                    "sites": [site_id.to_string()],
                    "min_free_disk_bytes": 1024
                }
            })),
        ))
        .await;
    assert_ok(st_cfg, &b_cfg, StatusCode::OK);

    let rev_en = env.get_plugin_rev("builtin.boost").await;
    env.enable_plugin("builtin.boost", rev_en, "r9-boost-en")
        .await;

    let rev1 = env.get_plugin_rev("builtin.boost").await;
    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev1}\"")),
                ("idempotency-key", "r9-boost-run"),
            ],
            Some(json!({ "kind": "boost.run", "payload": {} })),
        ))
        .await;
    let op = assert_ok(st_cmd, &b_cmd, StatusCode::ACCEPTED);
    let settled = support::poll_op(&env, op["operation_id"].as_str().unwrap()).await;
    assert_eq!(
        settled["state"], "failed",
        "boost.run targeting disabled downloader must fail rather than probing default_dl: {settled}"
    );
}

struct R9BrowserPage {
    current_origin: Arc<parking_lot::Mutex<String>>,
    navigated_urls: Arc<parking_lot::Mutex<Vec<String>>>,
    dom_acted_on_evil: Arc<AtomicBool>,
}

impl PluginBrowserPage for R9BrowserPage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }

    fn navigate(&mut self, url: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        self.navigated_urls.lock().push(url.to_string());
        *self.current_origin.lock() = "https://pt.example/nexus/ok".into();
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: b"<html><button id='btn'>Go</button><input id='inp'></html>".to_vec(),
        })
    }

    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_origin") {
            return Ok(json!(self.current_origin.lock().clone()));
        }
        if expr.contains("__cm_query") {
            return Ok(json!([{ "handle": "h1", "visible_text": "" }]));
        }
        if self
            .current_origin
            .lock()
            .starts_with("https://evil.example")
        {
            self.dom_acted_on_evil.store(true, Ordering::SeqCst);
        }
        Ok(json!(true))
    }

    fn cookie_header(&mut self, _: &str) -> Result<String, PluginBrowserError> {
        Ok(String::new())
    }

    fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        Ok(())
    }
}

fn seed_r9_site(store: &Store, name: &str, url: &str, pid: &str, enabled: bool) -> SiteId {
    let site_id = SiteId::new();
    store
        .insert_site(&Site {
            id: site_id,
            name: name.into(),
            url: url.into(),
            profile_id: format!("plugin:{pid}/fixture"),
            cookie: Some("c=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: Some("http://127.0.0.1:9222".into()),
            downloader_id: None,
            enabled,
            maintenance_via: MaintenanceVia::Http,
            browser_search: true,
            timezone: "UTC".into(),
        })
        .unwrap();
    site_id
}

fn build_r9_browser_pkg(pid: &str, key: &TestSigningKey) -> (Vec<u8>, String) {
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R9B\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\ncredential_binding = \"invocation-site\"\n[[components]]\nid = \"fixture\"\nentry = \"indexer.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"read:sites\", \"browser:search\", \"indexer:provide\"]\n[[components.indexers]]\nid = \"fixture\"\nname = \"Fixture\"\nkind = \"pt\"\ncapabilities = [\"search\", \"rss\", \"detail\", \"resolve-download\", \"user-stats\", \"categories\"]\nsite_schema = \"site.schema.json\"\n"
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    let pkg_bytes = build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("site.schema.json", schema.as_bytes()),
        ("indexer.wasm", INDEXER_WASM),
    ]);
    let sig = key.sign_package(&pkg_bytes);
    (pkg_bytes, sig)
}

#[tokio::test]
async fn browser_adapter_blocks_localhost_and_zero_self_api_and_disabled_sites() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(91);
    env.pin_trust_key(&key, "r9-bkey-1").await;
    let pid = "org.example.r9-browser-gate";
    let (pkg_bytes, sig) = build_r9_browser_pkg(pid, &key);

    let self_port = 18765u16;
    let store = env.state.store();
    let loc_site = seed_r9_site(
        &store,
        "LocalSelf",
        &format!("http://localhost:{self_port}/"),
        pid,
        true,
    );
    let zero_site = seed_r9_site(
        &store,
        "ZeroSelf",
        &format!("http://0.0.0.1:{self_port}/"),
        pid,
        true,
    );
    let dis_site = seed_r9_site(&store, "DisabledSite", "https://pt2.example/", pid, true);
    store.put_setting("cdp.enabled", "true").unwrap();

    let mut grants = vec![
        json!({"capability": "http", "scope": "Global", "sensitive": false}),
        json!({"capability": "read:sites", "scope": "Global", "sensitive": false}),
    ];
    for sid in [loc_site, zero_site, dis_site] {
        for cap in ["http:site-auth", "browser:search", "indexer:provide"] {
            grants.push(
                json!({"capability": cap, "scope": {"Site": sid.to_string()}, "sensitive": true}),
            );
        }
    }
    let inst = env
        .install_pkg(&pkg_bytes, Some(&sig), Value::Array(grants))
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r9-bg-en")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| {
            Ok(Box::new(R9BrowserPage {
                current_origin: Arc::new(parking_lot::Mutex::new("https://pt2.example/".into())),
                navigated_urls: Arc::new(parking_lot::Mutex::new(Vec::new())),
                dom_acted_on_evil: Arc::new(AtomicBool::new(false)),
            }))
        }),
    );
    let self_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self_port);
    let state_t = env
        .state
        .clone()
        .with_self_listen(self_addr)
        .with_plugin_http_policy_override(net_guard::TargetPolicy::production())
        .with_browser(browser)
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();

    for blocked_sid in [loc_site, zero_site] {
        let ep = ports
            .site_credential_epoch(&blocked_sid.to_string())
            .unwrap()
            .unwrap();
        let mut inv = Invocation::new(pid, Duration::from_secs(5));
        inv.generation = 1;
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(blocked_sid.to_string());
        inv.credential_epoch = Some(ep);
        inv.trust = PluginTrust::AdminTrusted;
        let _g = ports.begin_invocation(&inv).unwrap();
        let err = ports
            .browser_open(&inv, BrowserPurpose::Search)
            .err()
            .unwrap();
        assert_eq!(
            err.code,
            ErrorCode::PermissionDenied,
            "sid={blocked_sid} err={err:?}"
        );
        assert_eq!(err.detail, Some(ErrorDetail::BlockedAddress));
    }

    let ep_dis = ports
        .site_credential_epoch(&dis_site.to_string())
        .unwrap()
        .unwrap();
    let mut inv_dis = Invocation::new(pid, Duration::from_secs(5));
    inv_dis.generation = 1;
    inv_dis.role = GuestRole::Indexer;
    inv_dis.site_id = Some(dis_site.to_string());
    inv_dis.credential_epoch = Some(ep_dis);
    inv_dis.trust = PluginTrust::AdminTrusted;
    let _g_dis = ports.begin_invocation(&inv_dis).unwrap();
    let mut site_row = env.state.store().get_site(dis_site).unwrap().unwrap();
    site_row.enabled = false;
    env.state
        .store()
        .save_site(&site_row, store::ManualCredentialWrite::Unchanged)
        .unwrap();
    let dis_err = ports
        .browser_open(&inv_dis, BrowserPurpose::Search)
        .err()
        .unwrap();
    assert_eq!(dis_err.code, ErrorCode::StaleGeneration);
}

#[tokio::test]
async fn browser_adapter_preserves_subpath_and_blocks_cross_origin_wait_fill_click() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(92);
    env.pin_trust_key(&key, "r9-bkey-2").await;
    let pid = "org.example.r9-browser-nav";
    let (pkg_bytes, sig) = build_r9_browser_pkg(pid, &key);

    let store = env.state.store();
    let sub_site = seed_r9_site(&store, "SubSite", "https://pt.example/nexus", pid, true);
    store.put_setting("cdp.enabled", "true").unwrap();

    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": sub_site.to_string()}, "sensitive": true},
        {"capability": "browser:search", "scope": {"Site": sub_site.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": sub_site.to_string()}, "sensitive": true}
    ]);
    let inst = env.install_pkg(&pkg_bytes, Some(&sig), grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r9-bn-en")
        .await;

    let current_origin_shared = Arc::new(parking_lot::Mutex::new(
        "https://pt.example/nexus/".to_string(),
    ));
    let nav_urls = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let acted_evil = Arc::new(AtomicBool::new(false));
    let orig_c = current_origin_shared.clone();
    let nav_c = nav_urls.clone();
    let evil_c = acted_evil.clone();
    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(move |_| {
            Ok(Box::new(R9BrowserPage {
                current_origin: orig_c.clone(),
                navigated_urls: nav_c.clone(),
                dom_acted_on_evil: evil_c.clone(),
            }))
        }),
    );
    let state_t = env
        .state
        .clone()
        .with_browser(browser)
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();

    let ep_sub = ports
        .site_credential_epoch(&sub_site.to_string())
        .unwrap()
        .unwrap();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(sub_site.to_string());
    inv.credential_epoch = Some(ep_sub);
    inv.trust = PluginTrust::AdminTrusted;
    let _g = ports.begin_invocation(&inv).unwrap();
    let mut sess = ports.browser_open(&inv, BrowserPurpose::Search).unwrap();

    let _ = sess.navigate("/torrents.php?cat=1", "load").unwrap();
    assert_eq!(
        nav_urls.lock().last().map(String::as_str),
        Some("https://pt.example/nexus/torrents.php?cat=1")
    );
    assert_eq!(
        sess.navigate("/a/%2e%2e/b", "load").unwrap_err().code,
        ErrorCode::InvalidArgument
    );

    let handles = sess.query("#inp", 5).unwrap();
    *current_origin_shared.lock() = "https://evil.example/login".into();
    assert_eq!(
        sess.wait_for(Some("#inp"), "visible", 200)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        sess.fill(&handles[0].handle, BrowserFillValue::Text("hello".into()))
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        sess.click(&handles[0].handle).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert!(
        !acted_evil.load(Ordering::SeqCst),
        "must NOT evaluate wait_for/fill/click JS on cross-origin page"
    );
    sess.close().unwrap();
}
