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

struct R6BrowserPage;

impl PluginBrowserPage for R6BrowserPage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }

    fn navigate(&mut self, url: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        if url.ends_with("/gbk-html") {
            let mut body = b"<html>\xb1\xea\xcc\xe2".to_vec();
            body.extend_from_slice(
                b"<script>gbkEvil()</script><input type=\"password\" value=\"gbkPw11\"><p>GbkSafe</p></html>",
            );
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![("Content-Type".into(), "text/html; charset=gbk".into())],
                body,
            });
        }
        if url.ends_with("/raw-bin") {
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![("Content-Type".into(), "application/octet-stream".into())],
                body: vec![0xff, 0xfe, 0x00, 0x01],
            });
        }
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: concat!(
                r#"<svg><title/><script>svgEvil()</script><input type="password" value="svgPw1"><title>ok</title></svg>"#,
                r#"<math><style/><script>mathEvil()</script><input type="password" value="mathPw2"></math>"#,
                r#"<textarea/><script>xhtmlEvil()</script><input type="password" value="xhtmlPw3">"#,
                r#"</div x='a<b c="1'><script>endTagEvil()</script><input type="password" value="endTagPw4">"#,
                r#"<input type="&#000000112;assword" value="leadDecPw7">"#,
                r#"<input type="&#x0000070;assword" value="leadHexPw8">"#,
                r#"<input type="&#112assword" value="noSemiPw9">"#,
            )
            .as_bytes()
            .to_vec(),
        })
    }

    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_origin") {
            return Ok(json!("https://pt.example"));
        }
        if expr.contains("__cm_dom") {
            return Ok(json!(concat!(
                r#"</div title="unclosed <input type="password" value="endTagPw5">"#,
                r#"</div title="unclosed <input type=password value=endTagPw6>"#,
            )));
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

fn build_r6_browser_pkg(plugin_id: &str, key: &TestSigningKey) -> (Vec<u8>, String) {
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "R6 Browser Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R6 browser sanitization test"
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
async fn slice_e_r6_item3_browser_adapter_and_html_sanitizer_close_all_bypasses() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(631);
    env.pin_trust_key(&key, "k-631").await;

    let pid = "org.example.r6-browser-sanitize";
    let (pkg, sig) = build_r6_browser_pkg(pid, &key);
    let site_id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "R6BrowserSite".into(),
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
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-r6-bsan")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| Ok(Box::new(R6BrowserPage))),
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

    let _inv_guard = ports.begin_invocation(&inv).unwrap();
    let mut sess = ports.browser_open(&inv, BrowserPurpose::Search).unwrap();

    let r_gbk = sess.navigate("/gbk-html", "load").unwrap();
    let gbk_resp = sess.read_response(&r_gbk, 4096).unwrap();
    let gbk_text = String::from_utf8(gbk_resp.body).unwrap();
    assert!(!gbk_text.contains("gbkEvil"), "gbk_text={gbk_text}");
    assert!(!gbk_text.contains("gbkPw11"), "gbk_text={gbk_text}");
    assert!(gbk_text.contains("GbkSafe"), "gbk_text={gbk_text}");

    let r_bin = sess.navigate("/raw-bin", "load").unwrap();
    let bin_resp = sess.read_response(&r_bin, 4096).unwrap();
    assert_eq!(bin_resp.body, vec![0xff, 0xfe, 0x00, 0x01]);

    let r_chal = sess.navigate("/challengers", "load").unwrap();
    let chal_resp = sess.read_response(&r_chal, 8192).unwrap();
    let chal_text = String::from_utf8(chal_resp.body).unwrap();
    for secret in [
        "svgEvil",
        "svgPw1",
        "mathEvil",
        "mathPw2",
        "xhtmlEvil",
        "xhtmlPw3",
        "endTagEvil",
        "endTagPw4",
        "leadDecPw7",
        "leadHexPw8",
        "noSemiPw9",
    ] {
        assert!(
            !chal_text.contains(secret),
            "leaked `{secret}` in `{chal_text}`"
        );
    }

    let dom_out = sess.dom(4096).unwrap();
    assert!(!dom_out.contains("endTagPw5"), "dom_out={dom_out}");
    assert!(!dom_out.contains("endTagPw6"), "dom_out={dom_out}");

    sess.close().unwrap();
}
