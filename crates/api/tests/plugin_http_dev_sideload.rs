#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok,
    pkg::{TestSigningKey, build_task_package},
};
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::json;
use std::collections::BTreeMap;

const TASK_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");

fn build_dev_cmplugin(plugin_id: &str, version: &str, greeting: &str) -> Vec<u8> {
    let mut entries = BTreeMap::new();
    entries.insert(
        "plugin.toml".to_string(),
        format!(
            r#"manifest_version = 1
id = "{plugin_id}"
name = "Dev Sideload {greeting}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Dev"]
description = "Dev package"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "sync"
input_schema = "schemas/task.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 3600
"#
        )
        .into_bytes(),
    );
    entries.insert(
        "schemas/config.schema.json".to_string(),
        format!(
            r#"{{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{{"greeting":{{"type":"string","default":"{greeting}"}}}}}}"#
        )
        .into_bytes(),
    );
    entries.insert(
        "schemas/task.schema.json".to_string(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#
            .to_vec(),
    );
    entries.insert("components/main.wasm".to_string(), TASK_WASM.to_vec());
    build_deterministic_cmplugin_zip(&entries).expect("zip")
}

fn req_dev_raw(token: Option<&str>, bytes: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/api/v1/plugins/dev")
        .header("content-type", "application/octet-stream");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(bytes)).expect("req")
}

#[tokio::test]
async fn dev_sideload_enforces_rbac_dev_mode_and_reloads_dev_plugin() {
    let env = HttpEnv::new().await;
    let pid = "org.example.dev-sideload";
    let pkg_v1 = build_dev_cmplugin(pid, "1.0.0", "first");

    let (st_unauth, _, _) = env.send(req_dev_raw(None, pkg_v1.clone())).await;
    assert_eq!(st_unauth, StatusCode::UNAUTHORIZED);

    let (st_member, _, _) = env
        .send(req_dev_raw(Some(&env.member_token), pkg_v1.clone()))
        .await;
    assert_eq!(st_member, StatusCode::FORBIDDEN);

    let (st_disabled, _, body_disabled) = env.send(req_dev_raw(Some(TOKEN), pkg_v1.clone())).await;
    assert_err(
        st_disabled,
        &body_disabled,
        StatusCode::FORBIDDEN,
        "plugin.developer_mode_disabled",
    );

    env.enable_dev_mode().await;

    let (st_empty, _, body_empty) = env.send(req_dev_raw(Some(TOKEN), Vec::new())).await;
    assert_err(
        st_empty,
        &body_empty,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_package",
    );

    let (st_ok, _, body_ok) = env.send(req_dev_raw(Some(TOKEN), pkg_v1)).await;
    assert!(st_ok == StatusCode::OK || st_ok == StatusCode::CREATED);
    let data1 = assert_ok(st_ok, &body_ok, st_ok);
    assert_eq!(data1["id"], pid);
    let hash1 = data1["package_hash"]
        .as_str()
        .expect("package_hash")
        .to_string();
    let rev1 = data1["revision"].as_i64().expect("revision");

    // Reload same plugin_id + same version with changed config schema
    let pkg_v1_edit = build_dev_cmplugin(pid, "1.0.0", "second");
    let (st_reload, _, body_reload) = env.send(req_dev_raw(Some(TOKEN), pkg_v1_edit)).await;
    let data2 = assert_ok(st_reload, &body_reload, StatusCode::OK);
    assert_eq!(data2["id"], pid);
    assert_ne!(data2["package_hash"].as_str().unwrap(), hash1);
    assert!(data2["revision"].as_i64().unwrap() > rev1);
}

#[tokio::test]
async fn dev_sideload_refuses_to_overwrite_signed_trusted_plugin() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(505);
    env.pin_trust_key(&key, "Signed").await;

    let pid = "org.example.signed-locked";
    let signed_bytes = build_task_package(pid, "1.0.0", 1, false);
    let sig = key.sign_package(&signed_bytes);
    let detail = env.install_pkg(&signed_bytes, Some(&sig), json!([])).await;
    assert_eq!(detail["id"], pid);

    env.enable_dev_mode().await;
    let dev_bytes = build_dev_cmplugin(pid, "1.0.0", "overwrite");
    let (st_conflict, _, body_conflict) = env.send(req_dev_raw(Some(TOKEN), dev_bytes)).await;
    assert_err(
        st_conflict,
        &body_conflict,
        StatusCode::CONFLICT,
        "plugin.conflict",
    );
}

fn build_endpoint_pkg(plugin_id: &str) -> Vec<u8> {
    let ep_wasm = include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm");
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Endpoint HTTP Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Endpoint HTTP route fixture"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "ep"
entry = "components/endpoint.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv", "command:site.maintenance"]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components.endpoints]]
id = "kv-get"
method = "GET"
path = "/kv-get"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components.endpoints]]
id = "kv-put-ro"
method = "GET"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components.endpoints]]
id = "submit-ro"
method = "GET"
path = "/submit"
auth = "admin"
input_schema = "schemas/ep.schema.json"
"#
    );
    let mut entries = BTreeMap::new();
    entries.insert("plugin.toml".into(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#.to_vec(),
    );
    entries.insert(
        "schemas/ep.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"hello":{"type":"string"}}}"#.to_vec(),
    );
    entries.insert("components/endpoint.wasm".into(), ep_wasm.to_vec());
    build_deterministic_cmplugin_zip(&entries).expect("endpoint zip")
}

#[tokio::test]
async fn custom_plugin_http_endpoints_execute_wasm_and_enforce_admin() {
    use axum::body::to_bytes;
    use plugin_http_support::req_json;
    use tower::ServiceExt;

    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(709);
    env.pin_trust_key(&key, "k-709").await;
    let pid = "org.example.http-ep";
    let pkg = build_endpoint_pkg(pid);
    let installed = env
        .install_pkg(
            &pkg,
            Some(&key.sign_package(&pkg)),
            json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
        )
        .await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "en-ep")
        .await;

    let (st_mem, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/endpoints/kv-put"),
            Some(&env.member_token),
            &[],
            Some(json!({ "hello": "world" })),
        ))
        .await;
    assert_eq!(st_mem, StatusCode::FORBIDDEN);

    let req_put = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/plugins/{pid}/endpoints/kv-put"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"hello":"world"}"#))
        .unwrap();
    let resp_put = env.app.clone().oneshot(req_put).await.unwrap();
    assert_eq!(resp_put.status(), StatusCode::OK);
    assert_eq!(
        resp_put
            .headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );

    let req_get = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/plugins/{pid}/endpoints/kv-get"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let resp_get = env.app.clone().oneshot(req_get).await.unwrap();
    assert_eq!(resp_get.status(), StatusCode::OK);
    let get_bytes = to_bytes(resp_get.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&get_bytes[..], br#"{"hello":"world"}"#);

    let (st_miss, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/endpoints/undeclared"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_miss, StatusCode::NOT_FOUND);
}

fn build_minimal_ui_only_pkg(plugin_id: &str) -> Vec<u8> {
    build_ui_only_pkg_with_surface(plugin_id, "config", "./Config", &[])
}

fn build_ui_only_pkg_with_surface(
    plugin_id: &str,
    surface_kind: &str,
    expose: &str,
    caps: &[&str],
) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let remote_js = b"export const remote = true;\n";
    let sha: String = Sha256::digest(remote_js)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha,
            "size": remote_js.len(),
            "content_type": "application/javascript"
        }]
    }))
    .unwrap();
    let caps_json = serde_json::to_string(caps).unwrap();
    let remote_name = format!("rem_{}", plugin_id.replace(['.', '-'], "_"));
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "UI Only Endpoint Guard"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "UI-only fixture"
config_schema = "schemas/config.schema.json"
state_schema = 1

[ui]
kind = "federation-react"
entry = "ui/assets/remoteEntry.js"
remote_name = "{remote_name}"
asset_manifest = "ui/asset-manifest.json"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = {caps_json}

[[ui.surfaces]]
id = "main"
kind = "{surface_kind}"
expose = "{expose}"
title = "Main"
"#
    );
    let mut entries = BTreeMap::new();
    entries.insert("plugin.toml".into(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#.to_vec(),
    );
    entries.insert("ui/asset-manifest.json".into(), asset_manifest);
    entries.insert("ui/assets/remoteEntry.js".into(), remote_js.to_vec());
    build_deterministic_cmplugin_zip(&entries).expect("ui-only zip")
}

#[tokio::test]
async fn get_plugin_endpoints_forbid_mutations_and_ui_only_returns_404() {
    use plugin_http_support::req_json;

    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(712);
    env.pin_trust_key(&key, "k-712").await;

    let pid = "org.example.http-ep-ro";
    let pkg = build_endpoint_pkg(pid);
    let installed = env
        .install_pkg(
            &pkg,
            Some(&key.sign_package(&pkg)),
            json!([
                { "capability": "kv", "scope": "Global", "sensitive": false },
                { "capability": "command:site.maintenance", "scope": "Global", "sensitive": false }
            ]),
        )
        .await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "en-ep-ro")
        .await;

    let (st_kv, _, b_kv) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/endpoints/kv-put"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_kv, &b_kv, StatusCode::CONFLICT, "plugin.wrong_context");

    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/endpoints/submit"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_cmd, &b_cmd, StatusCode::CONFLICT, "plugin.wrong_context");

    let ui_pid = "org.example.ui-only-ep";
    let ui_pkg = build_minimal_ui_only_pkg(ui_pid);
    let ui_inst = env
        .install_pkg(&ui_pkg, Some(&key.sign_package(&ui_pkg)), json!([]))
        .await;
    env.enable_plugin(ui_pid, ui_inst["revision"].as_i64().unwrap(), "en-ui-only")
        .await;
    let (st_ui, _, b_ui) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{ui_pid}/endpoints/anything"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_ui,
        &b_ui,
        StatusCode::NOT_FOUND,
        "plugin.endpoint_not_found",
    );
}

async fn grant_ui_trust_for_pkg(env: &HttpEnv, pid: &str) {
    use plugin_http_support::req_json;
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let rev = prep["trust_revision"].as_i64().unwrap_or(0);
    let reauth = env.reauth("plugin-ui-trust").await;
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", &format!("ug-{pid}")),
            ],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK);
}

#[tokio::test]
async fn config_options_and_attributed_put_config_reject_draining_and_disabled_page_only_ui() {
    use plugin_http_support::req_json;
    use uuid::Uuid;

    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(713);
    env.pin_trust_key(&key, "k-713").await;

    let cfg_pid = "org.example.cfg-drain-gate";
    let cfg_pkg = build_ui_only_pkg_with_surface(cfg_pid, "config", "./Config", &["read:sites"]);
    let cfg_inst = env
        .install_pkg(&cfg_pkg, Some(&key.sign_package(&cfg_pkg)), json!([]))
        .await;
    env.enable_plugin(cfg_pid, cfg_inst["revision"].as_i64().unwrap(), "en-cfg-g")
        .await;
    grant_ui_trust_for_pkg(&env, cfg_pid).await;
    let rev = env.get_plugin_rev(cfg_pid).await;
    env.state
        .store()
        .begin_plugin_lifecycle(cfg_pid, rev, Uuid::new_v4(), false)
        .unwrap();

    let (st_drain, _, b_drain) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{cfg_pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "config.options", "input": { "kind": "sites" } })),
        ))
        .await;
    assert_err(
        st_drain,
        &b_drain,
        StatusCode::CONFLICT,
        "plugin.not_drained",
    );

    let pg_pid = "org.example.page-only-dis";
    let pg_pkg = build_ui_only_pkg_with_surface(pg_pid, "page", "./Page", &["read:sites"]);
    let pg_inst = env
        .install_pkg(&pg_pkg, Some(&key.sign_package(&pg_pkg)), json!([]))
        .await;
    let pg_hash = pg_inst["package_hash"].as_str().unwrap().to_string();
    let pg_gen = pg_inst["generation"].as_i64().unwrap().to_string();
    grant_ui_trust_for_pkg(&env, pg_pid).await;

    let (st_q, _, b_q) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pg_pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "config.options", "input": { "kind": "sites" } })),
        ))
        .await;
    assert_err(st_q, &b_q, StatusCode::FORBIDDEN, "plugin.not_enabled");

    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            &format!("/api/v1/plugins/{pg_pid}/config"),
            Some(TOKEN),
            &[
                ("if-match", "\"1\""),
                ("idempotency-key", "put-pg-dis"),
                ("x-plugin-package", pg_hash.as_str()),
                ("x-plugin-generation", pg_gen.as_str()),
            ],
            Some(json!({ "schema_revision": "1", "values": {}, "secrets": {} })),
        ))
        .await;
    assert_err(st_put, &b_put, StatusCode::FORBIDDEN, "plugin.not_enabled");
}
