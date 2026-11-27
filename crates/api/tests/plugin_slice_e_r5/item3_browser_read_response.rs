use std::sync::Arc;
use std::time::Duration;

use domain::{MaintenanceVia, Site, SiteId};
use indexer::{Browser, BrowserConfig, PluginBrowserError, PluginBrowserPage, PluginNavResponse};
use plugin_api::{BrowserPurpose, GuestRole, PluginTrust};
use plugin_host::Invocation;
use serde_json::{Value, json};

use crate::support::plugin_http_support::{
    HttpEnv,
    pkg::{TestSigningKey, build_stored_zip},
};

const INDEXER_WASM: &[u8] =
    include_bytes!("../../../plugin-host/tests/fixtures/indexer_fixture.wasm");

struct NavBodyFakePage;

impl PluginBrowserPage for NavBodyFakePage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }

    fn navigate(&mut self, url: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        if url.ends_with("/no-ct") {
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![],
                body: b"<noscript>hidden-token</noscript><input type=password value=hunter99><div>Visible</div>".to_vec(),
            });
        }
        if url.ends_with("/trunc") {
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![("Content-Type".into(), "application/xhtml+xml".into())],
                body: b"<script>012345678901234567890123456789</script><p>Hi</p>".to_vec(),
            });
        }
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: b"<html><body><script>steal()</script><input type=\"password\" value=\"s3cr3tPwd\"><p>Safe Title</p></body></html>".to_vec(),
        })
    }

    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_origin") {
            return Ok(json!("https://pt.example"));
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

fn build_browser_pkg(plugin_id: &str, key: &TestSigningKey) -> (Vec<u8>, String) {
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "R5 Browser Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R5 browser read_response test"
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

#[tokio::test]
async fn slice_e_r5_item3_browser_session_read_response_sanitizes_html_and_password_inputs() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(531);
    env.pin_trust_key(&key, "k-531").await;

    let pid = "org.example.r5-browser-resp";
    let (pkg, sig) = build_browser_pkg(pid, &key);
    let site_id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "R5BrowserSite".into(),
            url: "https://pt.example".into(),
            profile_id: format!("plugin:{pid}/fixture"),
            cookie: Some("c=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: Some("http://127.0.0.1:9222".into()),
            downloader_id: None,
            enabled: true,
            maintenance_via: MaintenanceVia::Http,
            browser_search: true,
            timezone: "UTC".into(),
        })
        .unwrap();
    env.state
        .store()
        .put_setting("cdp.enabled", "true")
        .unwrap();

    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "browser:search", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": site_id.to_string()}, "sensitive": true}
    ]);
    let inst = env.install_pkg(&pkg, Some(&sig), grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-r5-bresp")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| Ok(Box::new(NavBodyFakePage))),
    );
    let state_t = env
        .state
        .clone()
        .with_browser(browser)
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();
    let epoch = ports
        .site_credential_epoch(&site_id.to_string())
        .unwrap()
        .unwrap();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(site_id.to_string());
    inv.credential_epoch = Some(epoch);
    inv.trust = PluginTrust::AdminTrusted;

    let _guard = ports.begin_invocation(&inv).unwrap();
    let mut sess = ports.browser_open(&inv, BrowserPurpose::Search).unwrap();

    let r_html = sess.navigate("/html", "load").unwrap();
    let resp_html = sess.read_response(&r_html, 4096).unwrap();
    let body_html = String::from_utf8(resp_html.body).unwrap();
    assert!(!body_html.contains("<script>"), "body_html={body_html}");
    assert!(!body_html.contains("steal"), "body_html={body_html}");
    assert!(!body_html.contains("s3cr3tPwd"), "body_html={body_html}");
    assert!(body_html.contains("Safe Title"), "body_html={body_html}");
    assert!(!resp_html.truncated);

    let r_no_ct = sess.navigate("/no-ct", "load").unwrap();
    let resp_no_ct = sess.read_response(&r_no_ct, 4096).unwrap();
    let body_no_ct = String::from_utf8(resp_no_ct.body).unwrap();
    assert!(!body_no_ct.contains("hidden-token"), "body={body_no_ct}");
    assert!(!body_no_ct.contains("hunter99"), "body={body_no_ct}");
    assert!(body_no_ct.contains("Visible"), "body={body_no_ct}");

    let r_trunc = sess.navigate("/trunc", "load").unwrap();
    let resp_trunc = sess.read_response(&r_trunc, 20).unwrap();
    assert_eq!(resp_trunc.body, b"<p>Hi</p>");
    assert!(!resp_trunc.truncated);

    sess.close().unwrap();
}
