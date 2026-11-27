#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use axum::http::StatusCode;
use plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok, etag, location,
    pkg::{build_task_package, build_task_package_custom},
    req_json,
};
use serde_json::json;

#[tokio::test]
async fn config_schema_and_validate_fields() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.cfgv", "1.0.0", 1, true);
    env.install_pkg(&pkg, None, json!([])).await;

    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfgv/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema = assert_ok(st_sc, &b_sc, StatusCode::OK);
    let schema_rev = schema["schema_revision"].as_str().unwrap().to_string();

    let (st_g, h_g, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfgv/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cfg0 = assert_ok(st_g, &b_g, StatusCode::OK);
    assert_eq!(etag(&h_g), format!("\"{}\"", cfg0["revision"]));

    let (st_bad, _, b_bad) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.cfgv/config/validate",
            Some(TOKEN),
            &[],
            Some(json!({
                "schema_revision": schema_rev,
                "values": { "mode": "ok" },
                "secrets": { "api_secret": { "action": "replace", "value": "ab" } }
            })),
        ))
        .await;
    let err_bad = assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );
    let fields = err_bad["fields"].as_array().unwrap();
    assert!(!fields.is_empty());
    assert!(fields[0]["path"].is_string());

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.cfgv/config/validate",
            Some(TOKEN),
            &[],
            Some(json!({
                "schema_revision": schema_rev,
                "values": { "mode": "ok" },
                "secrets": { "api_secret": { "action": "replace", "value": "valid-secret-123" } }
            })),
        ))
        .await;
    assert_eq!(assert_ok(st_ok, &b_ok, StatusCode::OK)["valid"], true);
}

#[tokio::test]
async fn config_put_enforces_if_match_428_400_409() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.cfgim", "1.0.0", 1, true);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    let rev = detail["revision"].as_i64().unwrap();
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfgim/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema_rev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();
    let put_body = json!({
        "schema_revision": schema_rev, "values": { "mode": "one" },
        "secrets": { "api_secret": { "action": "replace", "value": "valid-secret-123" } }
    });

    let (st_428, _, b_428) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/org.example.cfgim/config",
            Some(TOKEN),
            &[("idempotency-key", "cfg-428")],
            Some(put_body.clone()),
        ))
        .await;
    assert_err(
        st_428,
        &b_428,
        StatusCode::PRECONDITION_REQUIRED,
        "plugin.precondition_required",
    );

    for bad_im in ["W/\"1\"", "1", "*", "\"\""] {
        let (st_400, _, b_400) = env
            .send(req_json(
                "PUT",
                "/api/v1/plugins/org.example.cfgim/config",
                Some(TOKEN),
                &[("if-match", bad_im), ("idempotency-key", "cfg-400")],
                Some(put_body.clone()),
            ))
            .await;
        assert_err(
            st_400,
            &b_400,
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
        );
    }

    let (st_409, _, b_409) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/org.example.cfgim/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{}\"", rev + 9)),
                ("idempotency-key", "cfg-409"),
            ],
            Some(put_body),
        ))
        .await;
    assert_err(
        st_409,
        &b_409,
        StatusCode::CONFLICT,
        "plugin.revision_conflict",
    );
}

#[tokio::test]
async fn config_put_secret_replace_keep_and_clear_actions() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.cfg", "1.0.0", 1, true);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    let rev = detail["revision"].as_i64().unwrap();
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfg/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema_rev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let (st_p1, h_p1, b_p1) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/org.example.cfg/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "cfg-put-1"),
            ],
            Some(json!({
                "schema_revision": schema_rev, "values": { "mode": "one" },
                "secrets": { "api_secret": { "action": "replace", "value": "valid-secret-123" } }
            })),
        ))
        .await;
    let c1 = assert_ok(st_p1, &b_p1, StatusCode::OK);
    let rev1 = c1["revision"].as_i64().unwrap();
    assert_eq!(etag(&h_p1), format!("\"{rev1}\""));
    assert_eq!(c1["secrets"]["api_secret"]["configured"], true);

    let (st_p2, h_p2, b_p2) = env
        .send(req_json("PUT", "/api/v1/plugins/org.example.cfg/config", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "cfg-put-2")], Some(json!({
            "schema_revision": schema_rev, "values": { "mode": "two" }, "secrets": { "api_secret": { "action": "keep" } }
        }))))
        .await;
    let c2 = assert_ok(st_p2, &b_p2, StatusCode::OK);
    let rev2 = c2["revision"].as_i64().unwrap();
    assert_eq!(etag(&h_p2), format!("\"{rev2}\""));
    assert_eq!(c2["values"]["mode"], "two");

    let (st_clr, _, b_clr) = env
        .send(req_json("PUT", "/api/v1/plugins/org.example.cfg/config", Some(TOKEN), &[("if-match", &format!("\"{rev2}\"")), ("idempotency-key", "cfg-put-clr")], Some(json!({
            "schema_revision": schema_rev, "values": { "mode": "two" }, "secrets": { "api_secret": { "action": "clear" } }
        }))))
        .await;
    assert_err(
        st_clr,
        &b_clr,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );
}

#[tokio::test]
async fn config_put_rejects_enabled_plugin_with_409_not_drained() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.cfgen", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(
        "org.example.cfgen",
        detail["revision"].as_i64().unwrap(),
        "en-cfgen",
    )
    .await;

    let (st_g, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfgen/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cfg = assert_ok(st_g, &b_g, StatusCode::OK);
    let rev = cfg["revision"].as_i64().unwrap();
    let srev = cfg["schema_revision"].as_str().unwrap();

    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/org.example.cfgen/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "cfg-en-put"),
            ],
            Some(json!({ "schema_revision": srev, "values": { "mode": "x" } })),
        ))
        .await;
    assert_err(st_put, &b_put, StatusCode::CONFLICT, "plugin.not_drained");
}

#[tokio::test]
async fn permissions_prepare_rejects_unknown_site_and_accepts_valid() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("Test Site");
    let pkg = build_task_package_custom(
        "org.example.permsp",
        "1.0.0",
        1,
        false,
        None,
        true,
        &["kv", "http:site-auth"],
    );
    env.install_pkg(&pkg, None, json!([])).await;

    let (st_p, h_p, b_p) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.permsp/permissions",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let pv = assert_ok(st_p, &b_p, StatusCode::OK);
    assert_eq!(etag(&h_p), format!("\"{}\"", pv["revision"]));

    let bad_grant = json!([{ "capability": "http:site-auth", "scope": { "Site": uuid::Uuid::new_v4() }, "sensitive": true }]);
    let (st_bad, _, b_bad) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.permsp/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": bad_grant })),
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.scope_invalid",
    );

    let grants = json!([{ "capability": "http:site-auth", "scope": { "Site": site_id }, "sensitive": true }]);
    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.permsp/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grants })),
        ))
        .await;
    let prep = assert_ok(st_ok, &b_ok, StatusCode::OK);
    assert!(!prep["consent_token"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn permissions_grant_and_revoke_drains_plugin() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("Grant Site");
    let pkg = build_task_package_custom(
        "org.example.perms",
        "1.0.0",
        1,
        false,
        None,
        true,
        &["kv", "http:site-auth"],
    );
    let rev = env.install_pkg(&pkg, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "http:site-auth", "scope": { "Site": site_id }, "sensitive": true }
    ]);
    let (st_pr, _, b_pr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.perms/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grants })),
        ))
        .await;
    let prep = assert_ok(st_pr, &b_pr, StatusCode::OK);
    let (st_gr, h_gr, b_gr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.perms/permissions/grant",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "perm-gr-1"),
            ],
            Some(json!({ "consent_token": prep["consent_token"], "grants": grants })),
        ))
        .await;
    assert_eq!(
        assert_ok(st_gr, &b_gr, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
    assert!(location(&h_gr).starts_with("/api/v1/plugin-operations/"));

    let rev_gr = env.get_plugin_rev("org.example.perms").await;
    env.enable_plugin("org.example.perms", rev_gr, "perm-en")
        .await;
    let rev_en = env.get_plugin_rev("org.example.perms").await;
    let (st_rv, h_rv, b_rv) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.perms/permissions/revoke", Some(TOKEN), &[("if-match", &format!("\"{rev_en}\"")), ("idempotency-key", "perm-rv-1")], Some(json!({
            "grants": [{ "capability": "http:site-auth", "scope": { "Site": site_id }, "sensitive": true }],
            "reason": "admin revoke"
        }))))
        .await;
    assert_eq!(
        assert_ok(st_rv, &b_rv, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
    assert!(location(&h_rv).starts_with("/api/v1/plugin-operations/"));
    let (st_d3, _, b_d3) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.perms",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(
        assert_ok(st_d3, &b_d3, StatusCode::OK)["lifecycle"],
        "disabled"
    );
}

#[tokio::test]
async fn if_match_rejects_signed_revisions_and_scope_rejects_unknown_fields() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("Strict Scope Site");
    let pkg = build_task_package("org.example.strictim", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    let rev = detail["revision"].as_i64().unwrap();
    assert_eq!(rev, 1);

    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.strictim/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema_rev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    for bad_im in ["\"+1\"", "\"+0\"", "\"-0\"", "\"-1\""] {
        let (st, _, body) = env
            .send(req_json(
                "PUT",
                "/api/v1/plugins/org.example.strictim/config",
                Some(TOKEN),
                &[
                    ("if-match", bad_im),
                    ("idempotency-key", &format!("im-{bad_im}")),
                ],
                Some(json!({ "schema_revision": schema_rev, "values": { "mode": "ok" } })),
            ))
            .await;
        assert_err(st, &body, StatusCode::BAD_REQUEST, "plugin.invalid_input");
    }

    let (st_unk, _, b_unk) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.strictim/state/reset/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "security_scopes": [{ "site_id": site_id, "unexpected": true }] })),
        ))
        .await;
    assert_err(st_unk, &b_unk, StatusCode::BAD_REQUEST, "request.invalid");
}

fn build_cfg_ui_zip(pid: &str, surface_kind: &str) -> (Vec<u8>, String) {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let entry = format!("export const ui = '{pid}';\n");
    let digest: String = Sha256::digest(entry.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": digest,
            "size": entry.len(),
            "content_type": "application/javascript"
        }]
    }))
    .unwrap();
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"UI Disabled\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"Test\"]\ndescription = \"UI\"\nconfig_schema = \"schemas/config.schema.json\"\nstate_schema = 1\n\n[ui]\nkind = \"federation-react\"\ncontract = \"1.0.0\"\nreact = \"19.3.0\"\nreact_dom = \"19.3.0\"\nremote_name = \"cfgDisRemote\"\nentry = \"ui/assets/remoteEntry.js\"\nasset_manifest = \"ui/asset-manifest.json\"\ncapabilities = [\"read:sites\"]\n\n[[ui.surfaces]]\nkind = \"{surface_kind}\"\nid = \"s1\"\nexpose = \"./S1\"\ntitle = \"S1\"\n"
    );
    let mut entries = BTreeMap::new();
    entries.insert("plugin.toml".to_string(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".to_string(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"label":{"type":"string","default":"x"}}}"#.to_vec(),
    );
    entries.insert("ui/asset-manifest.json".to_string(), asset_manifest);
    entries.insert("ui/assets/remoteEntry.js".to_string(), entry.into_bytes());
    let zip = plugin_package::build_deterministic_cmplugin_zip(&entries).unwrap();
    let hash = Sha256::digest(&zip)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (zip, hash)
}

async fn login_admin_cookie(env: &HttpEnv) -> String {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "username": "admin", "password": plugin_http_support::ADMIN_PASS }).to_string(),
        ))
        .unwrap();
    let resp = env.app.clone().oneshot(req).await.unwrap();
    resp.headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("mc_session=")
        .unwrap()
        .to_string()
}

async fn install_and_trust_ui(env: &HttpEnv, pid: &str, surface_kind: &str) -> (String, i64) {
    let (zip, hash) = build_cfg_ui_zip(pid, surface_kind);
    let detail = env.install_pkg(&zip, None, json!([])).await;
    let generation = detail["generation"].as_i64().unwrap();
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
    let reauth = env.reauth("plugin-ui-trust").await;
    let rev = prep["trust_revision"].as_i64().unwrap_or(0);
    let (st_gt, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", &format!("uit-{pid}")),
            ],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": hash,
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_eq!(st_gt, StatusCode::OK);
    (hash, generation)
}

#[tokio::test]
async fn disabled_config_surface_allows_assets_and_config_attribution() {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let admin_cookie = login_admin_cookie(&env).await;
    let (cfg_hash, gen1) = install_and_trust_ui(&env, "org.example.cfgui.dis", "config").await;

    let asset_resp = env
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/plugin-ui-assets/org.example.cfgui.dis/{cfg_hash}/ui/assets/remoteEntry.js"
                ))
                .header(header::COOKIE, format!("mc_session={admin_cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(asset_resp.status(), StatusCode::OK);

    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfgui.dis/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, _, b_cur) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cfgui.dis/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let crev = b_cur["data"]["revision"].as_i64().unwrap();
    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/org.example.cfgui.dis/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{crev}\"")),
                ("idempotency-key", "cfg-attr-dis"),
                ("x-plugin-package", &cfg_hash),
                ("x-plugin-generation", &gen1.to_string()),
            ],
            Some(json!({ "schema_revision": srev, "values": { "label": "saved-dis" } })),
        ))
        .await;
    assert_ok(st_put, &b_put, StatusCode::OK);

    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.cfgui.dis/commands",
            Some(TOKEN),
            &[
                ("if-match", "\"1\""),
                ("idempotency-key", "cmd-attr-dis"),
                ("x-plugin-package", &cfg_hash),
                ("x-plugin-generation", &gen1.to_string()),
            ],
            Some(json!({ "kind": "noop", "payload": {}, "targets": [{ "kind": "global" }], "evidence_ref": "ui" })),
        ))
        .await;
    assert_err(st_cmd, &b_cmd, StatusCode::FORBIDDEN, "plugin.not_enabled");
}

#[tokio::test]
async fn disabled_page_only_surface_blocks_assets() {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let admin_cookie = login_admin_cookie(&env).await;
    let (pg_hash, _) = install_and_trust_ui(&env, "org.example.pgui.dis", "page").await;

    let pg_resp = env
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/plugin-ui-assets/org.example.pgui.dis/{pg_hash}/ui/assets/remoteEntry.js"
                ))
                .header(header::COOKIE, format!("mc_session={admin_cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pg_resp.status(), StatusCode::FORBIDDEN);
}
