use std::sync::Arc;
use std::time::Duration;

use api::Store;
use domain::{MaintenanceVia, Site, SiteId};
use indexer::{Browser, BrowserConfig, PluginBrowserError, PluginBrowserPage, PluginNavResponse};
use plugin_api::{BrowserPurpose, ErrorCode, ErrorDetail, GuestRole, PluginTrust};
use plugin_host::Invocation;
use serde_json::{Value, json};

use crate::support::plugin_http_support::{
    HttpEnv,
    pkg::{TestSigningKey, build_stored_zip},
};

const INDEXER_WASM: &[u8] =
    include_bytes!("../../../plugin-host/tests/fixtures/indexer_fixture.wasm");

struct R8BrowserPage {
    current_origin: parking_lot::Mutex<String>,
    navigated_urls: Arc<parking_lot::Mutex<Vec<String>>>,
}

impl PluginBrowserPage for R8BrowserPage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }

    fn navigate(&mut self, url: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        self.navigated_urls.lock().push(url.to_string());
        if url.ends_with("/redirect-evil") {
            *self.current_origin.lock() = "https://evil-other.example/secret".into();
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: b"<html>CROSS_ORIGIN_SECRET_BODY</html>".to_vec(),
            });
        }
        *self.current_origin.lock() = "https://pt.example/ok".into();
        if url.ends_with("/utf16-bomless") {
            let html = "<html><script>utf16Evil()</script><input type=\"password\" value=\"utf16Pw99\"><p>Utf16Ok</p></html>";
            let mut utf16le = Vec::with_capacity(html.len() * 2);
            for u in html.encode_utf16() {
                utf16le.extend_from_slice(&u.to_le_bytes());
            }
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: utf16le,
            });
        }
        if url.ends_with("/iso2022jp") {
            let mut body = b"\x1b$B!<\x1b(Bdiv data-x=\"<script>stealIso()</script><input type='password' value='PWNED_ISO'>\">".to_vec();
            body.extend_from_slice(b"<p>IsoOk</p>");
            return Ok(PluginNavResponse {
                status: 200,
                headers: vec![(
                    "Content-Type".into(),
                    "text/html; charset=iso-2022-jp".into(),
                )],
                body,
            });
        }
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: b"<html><p>Ok</p></html>".to_vec(),
        })
    }

    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_origin") {
            return Ok(json!(self.current_origin.lock().clone()));
        }
        if expr.contains("__cm_dom") {
            return Ok(json!("<html>CROSS_ORIGIN_DOM</html>"));
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

fn seed_r8_browser_site(store: &Store, name: &str, url: &str, pid: &str) -> SiteId {
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
            enabled: true,
            maintenance_via: MaintenanceVia::Http,
            browser_search: true,
            timezone: "UTC".into(),
        })
        .unwrap();
    site_id
}

fn build_r8_browser_pkg(pid: &str, key: &TestSigningKey) -> (Vec<u8>, String) {
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R8B\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\ncredential_binding = \"invocation-site\"\n[[components]]\nid = \"fixture\"\nentry = \"indexer.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"read:sites\", \"browser:search\", \"indexer:provide\"]\n[[components.indexers]]\nid = \"fixture\"\nname = \"Fixture\"\nkind = \"pt\"\ncapabilities = [\"search\", \"rss\", \"detail\", \"resolve-download\", \"user-stats\", \"categories\"]\nsite_schema = \"site.schema.json\"\n"
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
async fn browser_adapter_decodes_charsets_enforces_origin_after_redirect_and_joins_query_site_urls()
{
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(88);
    env.pin_trust_key(&key, "r8-bkey").await;
    let pid = "org.example.r8-browser";
    let (pkg_bytes, sig) = build_r8_browser_pkg(pid, &key);
    let site_id = seed_r8_browser_site(
        env.state.store().as_ref(),
        "QuerySite",
        "https://pt.example/?lang=en",
        pid,
    );
    let meta_site_id = seed_r8_browser_site(
        env.state.store().as_ref(),
        "MetadataSite",
        "http://169.254.169.254/",
        pid,
    );
    env.state
        .store()
        .put_setting("cdp.enabled", "true")
        .unwrap();

    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "browser:search", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "http:site-auth", "scope": {"Site": meta_site_id.to_string()}, "sensitive": true},
        {"capability": "browser:search", "scope": {"Site": meta_site_id.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": meta_site_id.to_string()}, "sensitive": true}
    ]);
    let inst = env.install_pkg(&pkg_bytes, Some(&sig), grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r8-b-en")
        .await;

    let nav_urls = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let nav_urls_c = nav_urls.clone();
    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(move |_| {
            Ok(Box::new(R8BrowserPage {
                current_origin: parking_lot::Mutex::new("https://pt.example/".into()),
                navigated_urls: nav_urls_c.clone(),
            }))
        }),
    );
    let state_t = env
        .state
        .clone()
        .with_browser(browser)
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();

    let ep_meta = ports
        .site_credential_epoch(&meta_site_id.to_string())
        .unwrap()
        .unwrap();
    let mut inv_meta = Invocation::new(pid, Duration::from_secs(5));
    inv_meta.generation = 1;
    inv_meta.role = GuestRole::Indexer;
    inv_meta.site_id = Some(meta_site_id.to_string());
    inv_meta.credential_epoch = Some(ep_meta);
    inv_meta.trust = PluginTrust::AdminTrusted;
    let g_meta = ports.begin_invocation(&inv_meta).unwrap();
    let meta_err = ports
        .browser_open(&inv_meta, BrowserPurpose::Search)
        .err()
        .unwrap();
    assert_eq!(meta_err.code, ErrorCode::PermissionDenied);
    assert_eq!(meta_err.detail, Some(ErrorDetail::BlockedAddress));
    drop(g_meta);

    let ep = ports
        .site_credential_epoch(&site_id.to_string())
        .unwrap()
        .unwrap();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(site_id.to_string());
    inv.credential_epoch = Some(ep);
    inv.trust = PluginTrust::AdminTrusted;
    let _g = ports.begin_invocation(&inv).unwrap();
    let mut sess = ports.browser_open(&inv, BrowserPurpose::Search).unwrap();

    let r16 = sess.navigate("/utf16-bomless", "load").unwrap();
    assert_eq!(
        nav_urls.lock().last().map(String::as_str),
        Some("https://pt.example/utf16-bomless")
    );
    let body16 = String::from_utf8(sess.read_response(&r16, 4096).unwrap().body).unwrap();
    assert!(
        !body16.contains("utf16Evil")
            && !body16.contains("utf16Pw99")
            && body16.contains("Utf16Ok")
    );

    let riso = sess.navigate("/iso2022jp", "load").unwrap();
    let body_iso = String::from_utf8(sess.read_response(&riso, 4096).unwrap().body).unwrap();
    assert!(!body_iso.contains("stealIso") && !body_iso.contains("PWNED_ISO"));

    assert_eq!(
        sess.navigate("/redirect-evil", "load").unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        sess.dom(4096).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    sess.close().unwrap();
}
