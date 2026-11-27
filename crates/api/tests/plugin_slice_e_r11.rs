#[path = "builtin_boost/support.rs"]
pub(crate) mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use api::Store;
use axum::http::StatusCode;
use domain::{DownloaderId, MaintenanceVia, Site, SiteId};
use hooks::Gate;
use indexer::{
    Browser, BrowserConfig, MaintenanceAction, MaintenanceCallContext, PluginBrowserError,
    PluginBrowserPage, PluginNavResponse,
};
use plugin_api::{
    BrowserFillValue, BrowserPurpose, Candidate, DownloadBefore, DownloadPlanView, ErrorCode,
    ErrorDetail, GateInput, GatePayload, GuestRole, HookContext, PluginTrust,
};
use plugin_host::Invocation;
use serde_json::{Value, json};
use support::plugin_http_support::{
    HttpEnv, TOKEN, assert_err,
    pkg::{self, TestSigningKey, build_stored_zip},
    req_json,
};
use uuid::Uuid;

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");

struct SecretSimPage {
    filled_secret: bool,
}

impl SecretSimPage {
    fn new() -> Self {
        Self {
            filled_secret: false,
        }
    }
}

impl PluginBrowserPage for SecretSimPage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }
    fn navigate(&mut self, _: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: b"<html><body><input id='tok' type='text' value='init'></body></html>".to_vec(),
        })
    }
    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_query") {
            if self.filled_secret && expr.contains("data-cm-secret") {
                return Ok(json!([{ "handle": "cmh-1", "visible_text": "" }]));
            }
            return Ok(json!([{
                "handle": "cmh-1",
                "visible_text": if self.filled_secret { "TOP_SECRET_API_KEY" } else { "init" }
            }]));
        }
        if expr.contains("__cm_fill") {
            if expr.contains("TOP_SECRET_API_KEY") {
                self.filled_secret = true;
            }
            return Ok(json!(true));
        }
        if expr.contains("__cm_dom") {
            if expr.contains("[data-cm-secret]") {
                return Ok(json!(
                    "<html><body><input id=\"tok\" type=\"text\" data-cm-secret=\"1\" value=\"\"></body></html>"
                ));
            }
            return Ok(json!(
                "<html><body><input id=\"tok\" type=\"text\" value=\"TOP_SECRET_API_KEY\"></body></html>"
            ));
        }
        if expr.contains("__cm_text") {
            return Ok(json!(if expr.contains("data-cm-secret") {
                ""
            } else {
                "TOP_SECRET_API_KEY"
            }));
        }
        if expr.contains("__cm_attr") {
            return Ok(json!(if expr.contains("data-cm-secret") {
                ""
            } else {
                "TOP_SECRET_API_KEY"
            }));
        }
        if expr.contains("window.location.href") {
            return Ok(json!("https://pt-r11.example/login"));
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

fn build_r11_indexer_pkg(pid: &str) -> Vec<u8> {
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R11B\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\ncredential_binding = \"invocation-site\"\n[[components]]\nid = \"fixture\"\nentry = \"indexer.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"read:sites\", \"browser:search\", \"indexer:provide\"]\n[[components.indexers]]\nid = \"fixture\"\nname = \"Fixture\"\nkind = \"pt\"\ncapabilities = [\"search\", \"rss\", \"detail\", \"resolve-download\", \"user-stats\", \"categories\"]\nsite_schema = \"site.schema.json\"\n"
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("site.schema.json", schema.as_bytes()),
        ("indexer.wasm", INDEXER_WASM),
    ])
}

fn insert_site_url(store: &Store, name: &str, url: &str, profile: &str, enabled: bool) -> SiteId {
    let id = SiteId::new();
    store
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: profile.into(),
            cookie: Some("c_secure_uid=1".into()),
            api_key: Some("TOP_SECRET_API_KEY".into()),
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: Some("http://127.0.0.1:19222".into()),
            downloader_id: None,
            enabled,
            maintenance_via: MaintenanceVia::Http,
            browser_search: true,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

#[tokio::test]
async fn browser_open_blocks_rfc1918_private_hosts_and_cdp_self_endpoint() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(111);
    env.pin_trust_key(&key, "r11-bkey").await;
    let pid = "org.example.r11-browser-ssrf";
    let pkg_bytes = build_r11_indexer_pkg(pid);
    let sig = key.sign_package(&pkg_bytes);
    let store = env.state.store();
    store.put_setting("cdp.enabled", "true").unwrap();
    let prof = format!("plugin:{pid}/fixture");
    let rfc1918_site = insert_site_url(&store, "RFC1918", "http://10.0.0.1/", &prof, true);
    let cdp_ip_site = insert_site_url(&store, "CdpIp", "http://127.0.0.1:19222/", &prof, true);
    let cdp_localhost = insert_site_url(
        &store,
        "CdpLocalhost",
        "http://localhost:19222/",
        &prof,
        true,
    );
    let mdns_site = insert_site_url(&store, "Mdns", "http://nas.local/", &prof, true);

    let mut grants = vec![
        json!({"capability": "http", "scope": "Global", "sensitive": false}),
        json!({"capability": "read:sites", "scope": "Global", "sensitive": false}),
    ];
    for sid in [rfc1918_site, cdp_ip_site, cdp_localhost, mdns_site] {
        for cap in ["http:site-auth", "browser:search", "indexer:provide"] {
            grants.push(
                json!({"capability": cap, "scope": {"Site": sid.to_string()}, "sensitive": true}),
            );
        }
    }
    let inst = env
        .install_pkg(&pkg_bytes, Some(&sig), Value::Array(grants))
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r11-bs-en")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:19222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| Ok(Box::new(SecretSimPage::new()))),
    );
    let state_prod = env
        .state
        .clone()
        .with_browser(browser.clone())
        .with_self_listen(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18765))
        .with_admin_trusted_plugins([pid]);
    let ports_prod = state_prod.plugins().host_ports();
    for sid in [rfc1918_site, cdp_ip_site] {
        let ep = ports_prod
            .site_credential_epoch(&sid.to_string())
            .unwrap()
            .unwrap();
        let mut inv = Invocation::new(pid, Duration::from_secs(5));
        inv.generation = 1;
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(sid.to_string());
        inv.credential_epoch = Some(ep);
        inv.trust = PluginTrust::AdminTrusted;
        let _g = ports_prod.begin_invocation(&inv).unwrap();
        let err = ports_prod
            .browser_open(&inv, BrowserPurpose::Search)
            .err()
            .expect("must block RFC1918 and CDP IP");
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{err:?}");
        assert_eq!(err.detail, Some(ErrorDetail::BlockedAddress), "{err:?}");
    }

    // Even when test policy overrides loopback for 127.0.0.1:19222, CDP endpoint and .local MUST still be blocked
    let state_lb = state_prod.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production()
            .with_test_loopback_socket(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19222)),
    );
    let ports_lb = state_lb.plugins().host_ports();
    for sid in [cdp_ip_site, cdp_localhost, mdns_site] {
        let ep = ports_lb
            .site_credential_epoch(&sid.to_string())
            .unwrap()
            .unwrap();
        let mut inv = Invocation::new(pid, Duration::from_secs(5));
        inv.generation = 1;
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(sid.to_string());
        inv.credential_epoch = Some(ep);
        inv.trust = PluginTrust::AdminTrusted;
        let _g = ports_lb.begin_invocation(&inv).unwrap();
        let err = ports_lb
            .browser_open(&inv, BrowserPurpose::Search)
            .err()
            .expect("must block CDP endpoint and .local even under test loopback override");
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{err:?}");
        assert_eq!(err.detail, Some(ErrorDetail::BlockedAddress), "{err:?}");
    }
}

#[tokio::test]
async fn browser_fill_credential_slot_redacts_dom_query_text_and_attribute() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(112);
    env.pin_trust_key(&key, "r11-fill-key").await;
    let pid = "org.example.r11-browser-fill";
    let pkg_bytes = build_r11_indexer_pkg(pid);
    let sig = key.sign_package(&pkg_bytes);
    let store = env.state.store();
    store.put_setting("cdp.enabled", "true").unwrap();
    let ok_site = insert_site_url(
        &store,
        "OkSite",
        "https://pt-r11.example/",
        &format!("plugin:{pid}/fixture"),
        true,
    );
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": ok_site.to_string()}, "sensitive": true},
        {"capability": "browser:search", "scope": {"Site": ok_site.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": ok_site.to_string()}, "sensitive": true}
    ]);
    let inst = env.install_pkg(&pkg_bytes, Some(&sig), grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r11-fill-en")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:19222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| Ok(Box::new(SecretSimPage::new()))),
    );
    let state_t = env
        .state
        .clone()
        .with_browser(browser)
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();
    let ok_ep = ports
        .site_credential_epoch(&ok_site.to_string())
        .unwrap()
        .unwrap();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(ok_site.to_string());
    inv.credential_epoch = Some(ok_ep);
    inv.trust = PluginTrust::AdminTrusted;
    let _g = ports.begin_invocation(&inv).unwrap();
    let mut sess = ports.browser_open(&inv, BrowserPurpose::Search).unwrap();
    sess.navigate("/login", "load").unwrap();
    let els = sess.query("#tok", 1).unwrap();
    sess.fill(
        &els[0].handle,
        BrowserFillValue::CredentialSlot("site-api-key".into()),
    )
    .unwrap();
    let dom = sess.dom(4096).unwrap();
    let txt = sess.text(&els[0].handle, 512).unwrap();
    let attr = sess.attribute(&els[0].handle, "value").unwrap();
    let re_els = sess.query("#tok", 1).unwrap();
    assert!(!dom.contains("TOP_SECRET_API_KEY"), "dom leaked: {dom}");
    assert!(!txt.contains("TOP_SECRET_API_KEY"), "text leaked: {txt}");
    assert_eq!(attr.as_deref(), Some(""));
    assert!(
        !re_els[0].visible_text.contains("TOP_SECRET_API_KEY"),
        "query leaked: {:?}",
        re_els[0]
    );
}

fn build_boost_gate_input() -> GateInput {
    GateInput {
        schema_version: 1,
        hook_id: "download.before".into(),
        handles: vec!["h1".into()],
        revision: 1,
        context: HookContext {
            invocation_id: "inv-r11".into(),
            cause_id: "r11".into(),
            plugin_id: String::new(),
            generation: 1,
            config_revision: 1,
            subject: "system".into(),
            site_id: None,
            subscribe_id: None,
            library_id: None,
            deadline_ms: 500,
            cancelled: false,
        },
        payload: GatePayload::DownloadBefore(Box::new(DownloadBefore {
            purpose: "boost".into(),
            candidate: Candidate {
                handle: "h1".into(),
                site_id: String::new(),
                provider: "demo".into(),
                generation: 1,
                title: "Big".into(),
                size: Some(1_000_000),
                seeders: Some(10),
                leechers: Some(1),
                published_at: None,
                free: Some(true),
                hr: Some(false),
                release: None,
                score: None,
                labels: vec![],
                revision: 1,
            },
            plan: DownloadPlanView {
                downloader_id: DownloaderId::new().to_string(),
                relative_directory: None,
                category: None,
                tags: vec![],
                paused: false,
                upload_limit: None,
                download_limit: None,
                sequential: false,
                first_last_piece: false,
            },
        })),
    }
}

#[tokio::test]
async fn builtin_boost_gate_and_maintenance_provider_enforce_live_plugin_authority() {
    let env = HttpEnv::new().await;
    support::save_boost_config(
        &env.state,
        json!({ "enforce_budget_on_all": true, "budget_bytes": 0 }),
    );

    let boost_pid = "builtin.boost";
    let pipe = api::build_hook_pipeline_for_test(&env.state, "download.before");
    let (before_drain, _) = pipe
        .gate_typed_with_trace("wf-r11-pre", &build_boost_gate_input())
        .unwrap();
    assert!(
        matches!(before_drain, Gate::Reject(_)),
        "sanity check: enabled boost gate with 1-byte budget rejects 1MB candidate"
    );

    let boost_en = env.state.store().get_plugin(boost_pid).unwrap().unwrap();
    env.state
        .store()
        .begin_plugin_lifecycle(boost_pid, boost_en.revision, Uuid::new_v4(), false)
        .unwrap();
    let (decision, _) = pipe
        .gate_typed_with_trace("wf-r11", &build_boost_gate_input())
        .unwrap();
    assert!(
        matches!(decision, Gate::Allow | Gate::Abstain),
        "Draining builtin.boost gate must abstain rather than rejecting downloads, got {decision:?}"
    );

    let maint_pid = "builtin.nexusphp-maintenance";
    let maint_en = env.state.store().get_plugin(maint_pid).unwrap().unwrap();
    let site_id = insert_site_url(
        env.state.store().as_ref(),
        "MaintSite",
        "https://pt-maint.example/",
        "nexusphp-default",
        true,
    );
    let site = env.state.store().get_site(site_id).unwrap().unwrap();
    let snap = env.state.indexer().snapshot();
    let entry = snap
        .maintenance("plugin:builtin.nexusphp-maintenance/default")
        .expect("builtin.nexusphp-maintenance provider");
    env.state
        .store()
        .begin_plugin_lifecycle(maint_pid, maint_en.revision, Uuid::new_v4(), false)
        .unwrap();
    let ctx = MaintenanceCallContext {
        fetcher: env.state.indexer().as_ref(),
        generation: 1,
    };
    assert!(
        entry
            .provider()
            .run(&ctx, &site, MaintenanceAction::VerifySession, "job-r11")
            .is_err(),
        "Draining builtin.nexusphp-maintenance provider must fail live generation/lifecycle check"
    );
}

fn build_r11_site_cmd_pkg(pid: &str) -> Vec<u8> {
    let manifest = format!(
        "manifest_version=1\nid=\"{pid}\"\nname=\"R11SD\"\nversion=\"1.0.0\"\napi=\"1.0.0\"\nlicense=\"MIT\"\nauthors=[\"T\"]\ndescription=\"r11\"\nconfig_schema=\"schemas/config.schema.json\"\nstate_schema=1\n[[components]]\nid=\"worker\"\nentry=\"components/worker.wasm\"\nworld=\"{}\"\npermissions=[\"read:sites\",\"command:indexer.search\",\"command:downloader.add\"]\n[[components.tasks]]\nid=\"sync\"\ninput_schema=\"schemas/task.schema.json\"\nschedule={{kind=\"interval\",seconds=3600}}\n",
        plugin_host::TASK_WORLD,
    );
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        (
            "schemas/config.schema.json",
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#,
        ),
        (
            "schemas/task.schema.json",
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#,
        ),
        ("components/worker.wasm", pkg::task_wasm()),
    ])
}

#[tokio::test]
async fn indexer_search_and_downloader_add_reject_disabled_and_missing_sites() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(1105);
    env.pin_trust_key(&key, "k-1105").await;
    let disabled_site = insert_site_url(
        env.state.store().as_ref(),
        "DisabledPT",
        "https://disabled-pt.example/",
        "nexusphp-default",
        false,
    );
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "EnabledQB".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:8080".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();

    let pid = "org.example.r11.sitedisabled";
    let pkg_bytes = build_r11_site_cmd_pkg(pid);
    env.install_pkg(
        &pkg_bytes,
        Some(&key.sign_package(&pkg_bytes)),
        json!([
            { "capability": "read:sites", "scope": "Global", "sensitive": false },
            { "capability": "command:indexer.search", "scope": "Global", "sensitive": false },
            { "capability": "command:downloader.add", "scope": "Global", "sensitive": false }
        ]),
    )
    .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-r11-sd").await;
    let rev = env.get_plugin_rev(pid).await;

    let (st_idx, _, b_idx) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "r11-idx-disabled"),
            ],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "site_id": disabled_site.to_string(), "keyword": "matrix" }
            })),
        ))
        .await;
    assert_err(st_idx, &b_idx, StatusCode::CONFLICT, "plugin.site_disabled");

    let (st_dl_dis, _, b_dl_dis) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "r11-dl-disabled"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "site_id": disabled_site.to_string(),
                    "download_ref": "https://disabled-pt.example/1.torrent"
                }
            })),
        ))
        .await;
    assert_err(
        st_dl_dis,
        &b_dl_dis,
        StatusCode::CONFLICT,
        "plugin.site_disabled",
    );

    let missing_site = SiteId::new();
    let (st_dl_miss, _, b_dl_miss) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "r11-dl-missing"),
            ],
            Some(json!({
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl_id.to_string(),
                    "site_id": missing_site.to_string(),
                    "download_ref": "https://disabled-pt.example/1.torrent"
                }
            })),
        ))
        .await;
    assert_err(
        st_dl_miss,
        &b_dl_miss,
        StatusCode::NOT_FOUND,
        "site.missing",
    );
}
