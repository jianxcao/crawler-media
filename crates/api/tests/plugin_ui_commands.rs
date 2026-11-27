#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use plugin_http_support::{HttpEnv, TOKEN, assert_ok, pkg::TestSigningKey, req_json};
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_cmd_ui_package(
    plugin_id: &str,
    version: &str,
    remote_name: &str,
    capabilities: &[&str],
) -> Vec<u8> {
    let remote_js = b"export const remote = true;\n";
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha256_hex(remote_js),
            "size": remote_js.len(),
            "content_type": "application/javascript"
        }]
    }))
    .unwrap();
    let caps_json = serde_json::to_string(capabilities).unwrap();
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Command UI Plugin"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "UI query/command test plugin"
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
kind = "page"
id = "main"
expose = "./Page"
title = "Query Main"
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
    build_deterministic_cmplugin_zip(&entries).expect("zip")
}

async fn grant_ui_trust(env: &HttpEnv, pid: &str) {
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
                ("idempotency-key", &format!("ug-{}", Uuid::new_v4())),
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
async fn contract_endpoint_returns_expected_versions() {
    let env = HttpEnv::new().await;
    let (st, _, body) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-ui-contract",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let c = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(c["contract"], "1.0.0");
    assert_eq!(c["plugin_ui_sdk"], "1.0.0");
    assert_eq!(c["plugin_ui_kit"], "1.0.0");
    assert_eq!(c["react"], "19.3.0");
    assert_eq!(c["react_dom"], "19.3.0");
}

#[tokio::test]
async fn ui_query_closed_set_redaction_capabilities_and_errors() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(601);
    env.pin_trust_key(&key, "k-601").await;
    let site_id = env.insert_site("PT Secret Demo");

    let pid = "org.example.uiq";
    let pkg = build_cmd_ui_package(
        pid,
        "1.0.0",
        "uiQueryRemote",
        &[
            "read:sites",
            "read:downloaders",
            "read:libraries",
            "read:catalog",
            "read:downloads",
            "read:history",
            "read:boost",
            "read:subscribes",
        ],
    );
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;
    let rev = env.get_plugin_rev(pid).await;

    // Disabled plugin rejected on ui/query
    let (st_dis, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "sites.list", "input": {} })),
        ))
        .await;
    assert!(matches!(
        st_dis,
        StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));

    env.enable_plugin(pid, rev, "en-uiq").await;

    // Untrusted UI rejected on ui/query
    let (st_untrust, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "sites.list", "input": {} })),
        ))
        .await;
    assert_eq!(st_untrust, StatusCode::FORBIDDEN);

    grant_ui_trust(&env, pid).await;

    // Member gets 403
    let (st_mem, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(&env.member_token),
            &[],
            Some(json!({ "name": "sites.list", "input": {} })),
        ))
        .await;
    assert_eq!(st_mem, StatusCode::FORBIDDEN);

    // sites.list returns sanitized site without cookie secret
    let (st_sites, _, b_sites) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "sites.list", "input": { "limit": 20 } })),
        ))
        .await;
    let sites = assert_ok(st_sites, &b_sites, StatusCode::OK);
    assert!(!sites.to_string().contains("site-secret-cookie"));
    let items = sites["items"].as_array().unwrap();
    assert!(items.iter().any(|s| s["id"] == site_id.to_string()));

    // Other closed-set queries work
    for (name, input) in [
        ("downloaders.list", json!({})),
        ("libraries.list", json!({})),
        ("filters.list", json!({})),
        ("subscribes.list", json!({})),
        ("downloads.list", json!({})),
        ("checkin.receipts", json!({ "site_id": site_id })),
        ("boost.snapshot", json!({})),
    ] {
        let (st_q, _, b_q) = env
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid}/ui/query"),
                Some(TOKEN),
                &[],
                Some(json!({ "name": name, "input": input })),
            ))
            .await;
        assert_ok(st_q, &b_q, StatusCode::OK);
    }

    assert_ui_query_error_cases(&env, &key, pid).await;
}

async fn assert_ui_query_error_cases(env: &HttpEnv, key: &TestSigningKey, pid: &str) {
    // indexer.cached with missing operation returns non-generic 404
    let (st_ic, _, b_ic) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "indexer.cached", "input": { "search_operation_id": Uuid::new_v4() } })),
        ))
        .await;
    assert_eq!(st_ic, StatusCode::NOT_FOUND);
    assert_ne!(b_ic["error"]["code"], "not_found");

    // Unknown query rejected
    let (st_unk, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "users.list_secrets", "input": {} })),
        ))
        .await;
    assert!(matches!(
        st_unk,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN
    ));

    // Missing capability (only read:sites) rejects boost.snapshot
    let pid_narrow = "org.example.uinarrow";
    let pkg_narrow = build_cmd_ui_package(pid_narrow, "1.0.0", "uiNarrowRemote", &["read:sites"]);
    env.install_pkg(&pkg_narrow, Some(&key.sign_package(&pkg_narrow)), json!([]))
        .await;
    let rev_n = env.get_plugin_rev(pid_narrow).await;
    env.enable_plugin(pid_narrow, rev_n, "en-narrow").await;
    grant_ui_trust(env, pid_narrow).await;
    let (st_cap, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_narrow}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "boost.snapshot", "input": {} })),
        ))
        .await;
    assert_eq!(st_cap, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn ui_handle_ttl_scoping_and_attribution_headers_on_commands() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(602);
    env.pin_trust_key(&key, "k-602").await;
    let site_id = env.insert_site("Attribution PT");

    let pid = "org.example.uiattr";
    let pkg1 = build_cmd_ui_package(
        pid,
        "1.0.0",
        "uiAttrRemote",
        &["read:sites", "command:indexer.search"],
    );
    let h1 = sha256_hex(&pkg1);
    let grant_cmd =
        json!([{ "capability": "command:indexer.search", "scope": "Global", "sensitive": false }]);
    env.install_pkg(&pkg1, Some(&key.sign_package(&pkg1)), grant_cmd.clone())
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-ua1").await;
    grant_ui_trust(&env, pid).await;

    // Resolve opaque UiHandle
    let (st_res, _, b_res) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[("x-plugin-package", &h1), ("x-plugin-generation", "1")],
            Some(json!({ "name": "handles.resolve", "input": { "kind": "site", "id": site_id } })),
        ))
        .await;
    let resolved = assert_ok(st_res, &b_res, StatusCode::OK);
    let handle = resolved["handle"].as_str().unwrap().to_string();
    assert!(!handle.contains(&site_id.to_string()));

    // Command succeeds with valid UiHandle & attribution headers (without requiring If-Match)
    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("idempotency-key", "cmd-ok-attr"),
                ("x-plugin-package", &h1),
                ("x-plugin-generation", "1"),
            ],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "site_handle": handle, "keyword": "Dune" }
            })),
        ))
        .await;
    let cmd_op = assert_ok(st_cmd, &b_cmd, StatusCode::ACCEPTED);
    assert!(cmd_op["operation_id"].as_str().is_some());

    // Raw DB ID passed in site_handle field must fail closed (400/403)
    let (st_raw, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[("idempotency-key", "cmd-raw-id")],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "site_handle": site_id.to_string(), "keyword": "Dune" }
            })),
        ))
        .await;
    assert!(matches!(
        st_raw,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN
    ));

    assert_cross_plugin_and_forged_headers_rejected(&env, &key, pid, &h1, &handle, grant_cmd).await;
}

async fn assert_cross_plugin_and_forged_headers_rejected(
    env: &HttpEnv,
    key: &TestSigningKey,
    pid: &str,
    h1: &str,
    handle: &str,
    grant_cmd: serde_json::Value,
) {
    // Cross-plugin handle rejection & tampered handle rejection
    let pid2 = "org.example.uiattr2";
    let pkg2 = build_cmd_ui_package(
        pid2,
        "1.0.0",
        "uiAttrRemote2",
        &["read:sites", "command:indexer.search"],
    );
    env.install_pkg(&pkg2, Some(&key.sign_package(&pkg2)), grant_cmd)
        .await;
    let rev2 = env.get_plugin_rev(pid2).await;
    env.enable_plugin(pid2, rev2, "en-ua2").await;
    grant_ui_trust(env, pid2).await;
    let (st_cross, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid2}/commands"),
            Some(TOKEN),
            &[("idempotency-key", "cmd-cross-plugin")],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "site_handle": handle, "keyword": "Dune" }
            })),
        ))
        .await;
    assert!(matches!(
        st_cross,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));

    // Forged X-Plugin-Generation -> 409; mismatched X-Plugin-Package -> 409
    let (st_bad_gen, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("idempotency-key", "cmd-bad-gen"),
                ("x-plugin-package", h1),
                ("x-plugin-generation", "999"),
            ],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "site_handle": handle, "keyword": "Dune" }
            })),
        ))
        .await;
    assert_eq!(st_bad_gen, StatusCode::CONFLICT);

    let (st_bad_pkg, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[
                ("x-plugin-package", &"0".repeat(64)),
                ("x-plugin-generation", "1"),
            ],
            Some(json!({ "name": "sites.list", "input": {} })),
        ))
        .await;
    assert!(matches!(
        st_bad_pkg,
        StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));
}

#[tokio::test]
async fn ui_handle_site_maintenance_owned_download_and_browser_availability() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(602);
    env.pin_trust_key(&key, "k-602").await;
    let site_id = env.insert_site("PT Maint Handle");
    let pid = "org.example.maintui";
    let pkg = build_cmd_ui_package(
        pid,
        "1.0.0",
        "maintUiRemote",
        &[
            "read:sites",
            "read:boost",
            "command:site.maintenance",
            "command:downloader.delete-task",
        ],
    );
    let sig = key.sign_package(&pkg);
    let grants = json!([
        { "capability": "read:sites", "scope": "Global", "sensitive": false },
        { "capability": "read:boost", "scope": "Global", "sensitive": false },
        { "capability": "command:site.maintenance", "scope": "Global", "sensitive": false },
        { "capability": "command:downloader.delete-task", "scope": "Global", "sensitive": false },
    ]);
    env.install_pkg(&pkg, Some(&sig), grants).await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-mu").await;
    grant_ui_trust(&env, pid).await;

    let (st_res, _, b_res) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[
                ("x-plugin-package", &sha256_hex(&pkg)),
                ("x-plugin-generation", "1"),
            ],
            Some(json!({ "name": "handles.resolve", "input": { "kind": "site", "id": site_id } })),
        ))
        .await;
    let site_handle = assert_ok(st_res, &b_res, StatusCode::OK)["handle"]
        .as_str()
        .unwrap()
        .to_string();

    // Valid site_handle resolves site_id cleanly for site.maintenance
    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("idempotency-key", "maint-handle-ok"),
                ("x-plugin-package", &sha256_hex(&pkg)),
                ("x-plugin-generation", "1"),
            ],
            Some(json!({
                "kind": "site.maintenance",
                "payload": { "site_handle": site_handle, "action": "check-in" }
            })),
        ))
        .await;
    assert_eq!(st_ok, StatusCode::ACCEPTED, "got {b_ok}");

    // Raw UUID passed as site_handle must fail closed
    let (st_raw, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[("idempotency-key", "maint-handle-raw")],
            Some(json!({
                "kind": "site.maintenance",
                "payload": { "site_handle": site_id.to_string(), "action": "check-in" }
            })),
        ))
        .await;
    assert!(matches!(
        st_raw,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN
    ));

    assert_owned_download_and_browser_availability(&env, pid, &sha256_hex(&pkg), site_id).await;
}

async fn assert_owned_download_and_browser_availability(
    env: &HttpEnv,
    pid: &str,
    pkg_hash: &str,
    site_id: domain::SiteId,
) {
    // owned_download handle resolves cleanly once task is owned by plugin
    let dl_id = domain::DownloaderId::new();
    env.state
        .store()
        .accept_download_operation(&store::download_operations::AcceptDownloadOperationInput {
            operation_id: "op-task-123".into(),
            idempotency_key: "ik-task-123".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            cause_id: "seed".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/task-123.torrent".into(),
            ownership_tag: "task-123".into(),
            info_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
            bytes_digest: Some("dig-123".into()),
            verified_bytes: Some(100),
            request_digest: "req-123".into(),
            now_utc: 1_700_000_000,
        })
        .unwrap();
    env.state
        .store()
        .record_download_ownership(&store::download_operations::RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "task-123".into(),
            info_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
            operation_id: "op-task-123".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            site_id: Some(site_id),
            enclosure: "https://pt.example/task-123.torrent".into(),
            ownership_state: store::download_operations::DownloadOwnershipState::Owned,
            provenance_kind: store::download_operations::DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(100),
            bytes_digest: Some("dig-123".into()),
            now_utc: 1_700_000_000,
        })
        .unwrap();
    let (st_dl, _, b_dl) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[("x-plugin-package", pkg_hash), ("x-plugin-generation", "1")],
            Some(json!({ "name": "handles.resolve", "input": { "kind": "owned_download", "id": "task-123" } })),
        ))
        .await;
    let dl_handle = assert_ok(st_dl, &b_dl, StatusCode::OK);
    assert!(dl_handle["handle"].as_str().unwrap().starts_with("uh1."));

    let (st_av, _, b_av) = env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/{pid}/browser-availability?site_id={site_id}&purpose=maintenance"
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let av = assert_ok(st_av, &b_av, StatusCode::OK);
    assert!(av["status"].is_string(), "missing status: {av}");
    assert!(
        av["missing_requirements"].is_array(),
        "missing missing_requirements: {av}"
    );
}

#[tokio::test]
async fn enabled_config_not_editable_put_409_and_checkin_invalid_config_400() {
    let env = HttpEnv::new().await;

    let (st_ui, _, b_ui) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.checkin/ui",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let ui = assert_ok(st_ui, &b_ui, StatusCode::OK);
    let cfg_surf = ui["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "config")
        .unwrap();
    assert_eq!(
        cfg_surf["editable"], false,
        "enabled config must be read-only"
    );

    let (st_cfg, _, b_cfg) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.checkin/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cfg = assert_ok(st_cfg, &b_cfg, StatusCode::OK);
    let rev = cfg["revision"].as_i64().unwrap();
    let schema_rev = cfg["schema_revision"].as_str().unwrap();

    let (st_locked, _, b_locked) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/builtin.checkin/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "chk-put-enabled"),
            ],
            Some(json!({
                "schema_revision": schema_rev,
                "values": { "interval_secs": 7200 },
                "secrets": {}
            })),
        ))
        .await;
    assert_eq!(st_locked, StatusCode::CONFLICT, "got {b_locked}");

    env.disable_plugin("builtin.checkin", rev, "chk-dis").await;
    let rev_dis = env.get_plugin_rev("builtin.checkin").await;
    let (st_ui_dis, _, b_ui_dis) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.checkin/ui",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let ui_dis = assert_ok(st_ui_dis, &b_ui_dis, StatusCode::OK);
    let cfg_surf_dis = ui_dis["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "config")
        .unwrap();
    assert_eq!(cfg_surf_dis["editable"], true);

    let (st_bad, _, b_bad) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/builtin.checkin/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", "chk-put-bad"),
            ],
            Some(json!({
                "schema_revision": schema_rev,
                "values": { "interval_secs": 10, "transport": "ftp", "sites": [] },
                "secrets": {}
            })),
        ))
        .await;
    assert_eq!(st_bad, StatusCode::BAD_REQUEST, "got {b_bad}");
    assert_eq!(
        b_bad["error"]["code"], "plugin.config_invalid",
        "got {b_bad}"
    );
}
