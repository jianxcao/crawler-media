#[path = "../builtin_boost/support.rs"]
mod support;

mod item1_admin_ops;
mod item2_trust_and_browser_check;
mod item3_browser_read_response;
mod item4_ui_asset_manifest;
mod item5_boost_cleanup_paths;
mod item6_boost_rss_grant;

use std::collections::BTreeMap;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::DownloaderId;
use downloader::{Downloader, PathMap};
use indexer::RegistrySnapshot;
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::plugins::CanonicalPluginRequest;
use support::NoopFetcher;
use support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use support::poll_op;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn http_canon_req(method: &str, path: &str, body: &Value) -> CanonicalPluginRequest {
    let bytes = serde_json::to_vec(body).unwrap();
    CanonicalPluginRequest {
        method: method.into(),
        path: path.into(),
        body_digest: Sha256::digest(&bytes).into(),
    }
}

pub(crate) fn build_ui_package(plugin_id: &str, remote_name: &str, caps: &[&str]) -> Vec<u8> {
    let remote_js = format!(
        "export const remote = '{remote_name}';\nexport function get() {{ return remote; }}\n"
    );
    let css = b".r5-card { color: inherit; }\n";
    let files = vec![
        json!({
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha256_hex(remote_js.as_bytes()),
            "size": remote_js.len(),
            "content_type": "application/javascript"
        }),
        json!({
            "path": "ui/assets/style.css",
            "sha256": sha256_hex(css),
            "size": css.len(),
            "content_type": "text/css"
        }),
    ];
    let asset_manifest = serde_json::to_vec(&json!({ "version": 1, "files": files })).unwrap();
    let caps_json = serde_json::to_string(caps).unwrap();
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "R5 UI Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R5 UI fixture"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
remote_name = "{remote_name}"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
capabilities = {caps_json}

[[ui.surfaces]]
kind = "config"
id = "settings"
expose = "./Config"
title = "Settings"

[[ui.surfaces]]
kind = "page"
id = "main"
expose = "./Page"
title = "Main"
"#
    );
    let entries = BTreeMap::from([
        ("plugin.toml".to_string(), manifest.into_bytes()),
        (
            "schemas/config.schema.json".to_string(),
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#.to_vec(),
        ),
        (
            "ui/assets/remoteEntry.js".to_string(),
            remote_js.into_bytes(),
        ),
        ("ui/assets/style.css".to_string(), css.to_vec()),
        ("ui/asset-manifest.json".to_string(), asset_manifest),
    ]);
    build_deterministic_cmplugin_zip(&entries).expect("ui zip")
}

pub(crate) fn env_with_dl_maps<D: Downloader + 'static>(
    dir: tempfile::TempDir,
    dl: Arc<D>,
    path_maps: Vec<PathMap>,
) -> (HttpEnv, domain::SiteId, DownloaderId) {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    let site_id = env.insert_site("PT-R5");
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "R5DL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps,
            is_default: true,
            enabled: true,
        })
        .unwrap();
    (env, site_id, dl_id)
}

pub(crate) async fn run_boost_cmd(env: &HttpEnv, idem: &str, kind: &str, payload: Value) -> Value {
    let rev = env.get_plugin_rev("builtin.boost").await;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": kind, "payload": payload })),
        ))
        .await;
    let op = assert_ok(st, &body, StatusCode::ACCEPTED);
    poll_op(env, op["operation_id"].as_str().unwrap()).await
}
