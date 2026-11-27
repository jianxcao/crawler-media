#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_api::{HOOK_CATALOG, HOOK_WORLD};
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, pkg, req_json};
use serde_json::{Value, json};
use tower::ServiceExt;

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

fn build_policy_package(id: &str, hooks_toml: &str, permissions: &[&str]) -> Vec<u8> {
    let perms = permissions
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
permissions = [{perms}]

{hooks_toml}
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

#[tokio::test]
async fn hook_catalog_enforces_admin_auth_and_returns_all_11_hooks() {
    let env = HttpEnv::new().await;

    let (st_unauth, _, b_unauth) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-hook-catalog",
            None,
            &[],
            None,
        ))
        .await;
    assert_err(
        st_unauth,
        &b_unauth,
        StatusCode::UNAUTHORIZED,
        "auth.required",
    );

    let (st_member, _, b_member) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-hook-catalog",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_member,
        &b_member,
        StatusCode::FORBIDDEN,
        "auth.forbidden",
    );

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-hook-catalog",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data = assert_ok(st_ok, &b_ok, StatusCode::OK);
    let items = data["items"].as_array().unwrap();
    assert_eq!(items.len(), HOOK_CATALOG.len());
    assert_eq!(data["total"].as_u64(), Some(11));

    let by_id: HashMap<&str, &Value> = items
        .iter()
        .map(|item| (item["id"].as_str().unwrap(), item))
        .collect();
    for entry in HOOK_CATALOG {
        let item = by_id[entry.id];
        assert_eq!(item["phase"].as_u64(), Some(u64::from(entry.phase)));
        assert!(item["failure_policy"].as_str().is_some());
        assert!(item["default_timeout_ms"].as_u64().is_some());
        let expected_wired = true;
        assert_eq!(item["wired"].as_bool(), Some(expected_wired));
    }
}

struct MockSiteFetcher {
    requests: Arc<Mutex<Vec<String>>>,
    items: Vec<(String, String)>,
}

impl Fetcher for MockSiteFetcher {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        self.requests.lock().unwrap().push(req.url.clone());
        let rows: Vec<Value> = self
            .items
            .iter()
            .enumerate()
            .map(|(idx, (id, name))| {
                json!({
                    "id": id,
                    "name": name,
                    "size": format!("{}", 1024 * (idx + 1)),
                    "seeders": "20",
                    "leechers": "2"
                })
            })
            .collect();
        Ok(json!({ "code": "0", "data": { "data": rows } }).to_string())
    }
}

async fn setup_search_policy_env(items: Vec<(String, String)>, site_names: &[&str]) -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let fetcher = Arc::new(MockSiteFetcher {
        requests: Arc::new(Mutex::new(Vec::new())),
        items,
    });
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        fetcher,
        dl,
    )
    .unwrap();
    let app = router(state.clone());
    for name in site_names {
        state
            .store()
            .insert_site(&Site {
                id: SiteId::new(),
                name: (*name).into(),
                url: format!("https://{name}.example/"),
                profile_id: "mteam".into(),
                cookie: None,
                api_key: Some("secret-api-key".into()),
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
    }
    let env = HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    };
    env.enable_dev_mode().await;
    let hooks_toml = "[[components.hooks]]\nid = \"search.before\"\npriority = 10\n\n[[components.hooks]]\nid = \"search.site.after\"\npriority = 10\n\n[[components.hooks]]\nid = \"search.after\"\npriority = 10\n\n[[components.hooks]]\nid = \"filter.after\"\npriority = 10\n";
    let pkg = build_policy_package(
        "org.example.search-policy",
        hooks_toml,
        &[
            "hooks:search.before",
            "hooks:search.site.after",
            "hooks:search.after",
            "hooks:filter.after",
        ],
    );
    let installed = env
        .install_pkg(
            &pkg,
            None,
            json!([
                { "capability": "hooks:search.before", "scope": "Global", "sensitive": false },
                { "capability": "hooks:search.site.after", "scope": "Global", "sensitive": false },
                { "capability": "hooks:search.after", "scope": "Global", "sensitive": false },
                { "capability": "hooks:filter.after", "scope": "Global", "sensitive": false }
            ]),
        )
        .await;
    let rev = installed["revision"].as_i64().unwrap();
    env.enable_plugin("org.example.search-policy", rev, "en-search-pol")
        .await;
    env
}

#[tokio::test]
async fn search_pipeline_wires_search_before_site_after_after_filter_after_and_sanitized_trace() {
    let env = setup_search_policy_env(
        vec![
            ("101".into(), "A".into()),
            ("102".into(), "B.Movie.2024.1080p".into()),
        ],
        &["Site1", "Site2"],
    )
    .await;

    // 1. Normal search: policy.wasm drops title == "A" on search.site.after & boosts surviving B on filter.after!
    let (st, _, body) = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=Matrix",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data = assert_ok(st, &body, StatusCode::OK);
    let items = data["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(
        items.iter().all(|i| i["title"].as_str() != Some("A")),
        "search.site.after / search.after must have dropped title A: {items:?}"
    );
    let trace = data["plugin_trace"].as_array().unwrap();
    let hook_ids: Vec<&str> = trace.iter().filter_map(|t| t["hook_id"].as_str()).collect();
    assert!(hook_ids.contains(&"search.before"));
    assert!(hook_ids.contains(&"search.site.after"));
    assert!(hook_ids.contains(&"search.after"));
    assert!(hook_ids.contains(&"filter.after"));

    // 2. SSE stream surfaces plugin_trace in `done`.
    let resp = env
        .app
        .clone()
        .oneshot(req_json(
            "GET",
            "/api/v1/search/torrents/stream?keyword=Matrix",
            Some(TOKEN),
            &[],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        text.contains("plugin_trace"),
        "SSE done event should include plugin_trace: {text}"
    );

    // 3. Gate reject on search.before aborts search with 403 plugin.rejected + trace.
    let (st_rej, _, b_rej) = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=reject-me",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_rej, &b_rej, StatusCode::FORBIDDEN, "plugin.rejected");

    // 4. Filter KV probe without state:kv permission in search.before is denied and fails open.
    let (st_kv, _, b_kv) = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=kv",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data_kv = assert_ok(st_kv, &b_kv, StatusCode::OK);
    let trace_kv = data_kv["plugin_trace"].as_array().unwrap();
    let sb_step = trace_kv
        .iter()
        .find(|t| t["hook_id"].as_str() == Some("search.before"))
        .and_then(|t| t["plugin_steps"].as_array())
        .and_then(|steps| steps.first())
        .expect("search.before step trace");
    assert_eq!(sb_step["status"].as_str(), Some("rolled_back"));
}

#[tokio::test]
async fn manual_search_filter_after_rescore_sorts_descending_unless_set_order_overrides() {
    let env_rescore = setup_search_policy_env(
        vec![
            ("201".into(), "First.RescoreSecond.1080p".into()),
            ("202".into(), "Second.RescoreSecond.1080p".into()),
        ],
        &["SiteSort1"],
    )
    .await;
    let (st1, _, body1) = env_rescore
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=RescoreSecond",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data1 = assert_ok(st1, &body1, StatusCode::OK);
    let titles1: Vec<&str> = data1["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["title"].as_str())
        .collect();
    assert_eq!(
        titles1,
        vec!["Second.RescoreSecond.1080p", "First.RescoreSecond.1080p"],
        "filter.after Rescore without SetOrder must sort descending by score"
    );

    let env_order = setup_search_policy_env(
        vec![
            ("301".into(), "First.SetOrderKeep.1080p".into()),
            ("302".into(), "Second.SetOrderKeep.1080p".into()),
        ],
        &["SiteSort2"],
    )
    .await;
    let (st2, _, body2) = env_order
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=SetOrderKeep",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data2 = assert_ok(st2, &body2, StatusCode::OK);
    let titles2: Vec<&str> = data2["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["title"].as_str())
        .collect();
    assert_eq!(
        titles2,
        vec!["First.SetOrderKeep.1080p", "Second.SetOrderKeep.1080p"],
        "explicit SetOrder must override score sorting"
    );
}

#[tokio::test]
async fn search_plugin_trace_redacts_cookie_pass_and_escaped_json_secrets() {
    let env = setup_search_policy_env(
        vec![("401".into(), "Matrix.1999.1080p".into())],
        &["SiteSecret"],
    )
    .await;
    let (st, _, body) = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=secret-error",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data = assert_ok(st, &body, StatusCode::OK);
    let rendered = data["plugin_trace"].to_string();
    assert!(!rendered.contains("SECRET_COOKIE_PASS"), "{rendered}");
    assert!(!rendered.contains("SECRET_JSON_PK"), "{rendered}");
    assert!(!rendered.contains("uid=1"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
}

#[tokio::test]
async fn runtime_hooks_persist_audit_events_in_plugin_audit() {
    let env = setup_search_policy_env(
        vec![("501".into(), "Matrix.1999.1080p".into())],
        &["AuditSite"],
    )
    .await;
    let _ = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=reject-me",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (st_ok, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/search/torrents?keyword=Matrix",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_ok, StatusCode::OK);

    let (st_aud, _, b_aud) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.search-policy/audit?limit=50",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (items, _) = plugin_http_support::assert_paged(st_aud, &b_aud);
    assert!(
        items
            .iter()
            .any(|e| e["action"] == "search.before" && e["canonical_method"] == "HOOK"),
        "expected search.before HOOK audit entry, got: {items:?}"
    );
    assert!(
        items
            .iter()
            .any(|e| e["action"] == "search.after" && e["canonical_method"] == "HOOK"),
        "expected search.after HOOK audit entry, got: {items:?}"
    );
}
