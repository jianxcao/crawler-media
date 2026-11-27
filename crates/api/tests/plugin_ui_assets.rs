#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::collections::BTreeMap;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, pkg::TestSigningKey, req_json,
};
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn asset_entry(path: &str, bytes: &[u8], mime: &str) -> Value {
    json!({ "path": path, "sha256": sha256_hex(bytes), "size": bytes.len(), "content_type": mime })
}

fn build_ui_package(
    plugin_id: &str,
    version: &str,
    remote_name: &str,
    capabilities: &[&str],
    surfaces_toml: &str,
    extra_assets: &[(&str, &[u8], &str)],
) -> Vec<u8> {
    let remote_js = format!(
        "export const remote = '{remote_name}';\nexport const ver = '{version}';\nexport function get() {{ return remote; }}\n"
    );
    let css = b".plugin-card { color: inherit; }\n";
    let mut files = vec![
        asset_entry(
            "ui/assets/remoteEntry.js",
            remote_js.as_bytes(),
            "application/javascript",
        ),
        asset_entry("ui/assets/style.css", css, "text/css"),
    ];
    let mut entries = BTreeMap::from([
        (
            "ui/assets/remoteEntry.js".to_string(),
            remote_js.into_bytes(),
        ),
        ("ui/assets/style.css".to_string(), css.to_vec()),
    ]);
    for (path, bytes, mime) in extra_assets {
        files.push(asset_entry(path, bytes, mime));
        entries.insert((*path).to_string(), (*bytes).to_vec());
    }
    let asset_manifest = serde_json::to_vec(&json!({ "version": 1, "files": files })).unwrap();
    let caps_json = serde_json::to_string(capabilities).unwrap();
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "UI Test Plugin"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Federated UI fixture"
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

{surfaces_toml}
"#
    );
    entries.insert("plugin.toml".to_string(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".to_string(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"label":{"type":"string","default":"x"}}}"#.to_vec(),
    );
    entries.insert("ui/asset-manifest.json".to_string(), asset_manifest);
    build_deterministic_cmplugin_zip(&entries).expect("zip")
}

fn default_surfaces() -> String {
    r#"[[ui.surfaces]]
kind = "config"
id = "settings"
expose = "./Config"
title = "Settings"

[[ui.surfaces]]
kind = "page"
id = "main"
expose = "./Page"
title = "Plugin Main"
icon = "puzzle"
"#
    .to_string()
}

async fn login_cookie(
    env: &HttpEnv,
    username: &str,
    password: &str,
    https: bool,
) -> (String, HeaderMap) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/json");
    if https {
        builder = builder.header("x-forwarded-proto", "https");
    }
    let req = builder
        .body(Body::from(
            json!({ "username": username, "password": password }).to_string(),
        ))
        .unwrap();
    let resp = env.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers().clone();
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .expect("mc_session Set-Cookie")
        .to_str()
        .unwrap()
        .to_string();
    let cookie_val = set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("mc_session=")
        .unwrap()
        .to_string();
    (cookie_val, headers)
}

async fn send_raw(env: &HttpEnv, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = env.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

async fn grant_ui_trust_via_http(env: &HttpEnv, pid: &str, package_hash: Option<&str>) -> Value {
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
    let hash = package_hash.unwrap_or_else(|| prep["package_hash"].as_str().unwrap());
    let idem = format!("uit-grant-{}", Uuid::new_v4());
    let if_match = format!("\"{rev}\"");
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", &if_match), ("idempotency-key", &idem)],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": hash,
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK)
}

#[tokio::test]
async fn cookie_auth_login_logout_rbac_and_public_images_remain_unchanged() {
    let env = HttpEnv::new().await;
    let (admin_cookie, http_hdrs) = login_cookie(&env, "admin", ADMIN_PASS, false).await;
    let sc_http = http_hdrs.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(sc_http.contains("HttpOnly"));
    assert!(sc_http.contains("SameSite=Lax"));
    assert!(!sc_http.contains("Secure"));

    let (_, https_hdrs) = login_cookie(&env, "admin", ADMIN_PASS, true).await;
    let sc_https = https_hdrs
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(sc_https.contains("Secure"));

    let key = TestSigningKey::generate(501);
    env.pin_trust_key(&key, "k-501").await;
    let pid = "org.example.cookieauth";
    let pkg = build_ui_package(
        pid,
        "1.0.0",
        "cookieAuthRemote",
        &["read:sites"],
        &default_surfaces(),
        &[],
    );
    let hash = sha256_hex(&pkg);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-ca").await;
    grant_ui_trust_via_http(&env, pid, Some(&hash)).await;

    let asset_uri = format!("/api/v1/plugin-ui-assets/{pid}/{hash}/ui/assets/remoteEntry.js");
    // Anonymous fails with 401 JSON (not HTML)
    let (st_anon, _, b_anon) = send_raw(
        &env,
        Request::builder()
            .method("GET")
            .uri(&asset_uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_anon, StatusCode::UNAUTHORIZED);
    let v_anon: Value = serde_json::from_slice(&b_anon).unwrap();
    assert_eq!(v_anon["ok"], false);

    // Bearer-only (no cookie) fails with 401 on /plugin-ui-assets/*, works on /plugins/:id/ui
    let (st_bo_asset, _, _) =
        send_raw(&env, req_json("GET", &asset_uri, Some(TOKEN), &[], None)).await;
    assert_eq!(st_bo_asset, StatusCode::UNAUTHORIZED);
    let (st_bo_ui, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_bo_ui, StatusCode::OK);

    // Member gets 403 on UI endpoints & assets
    let (member_cookie, _) = login_cookie(&env, "plugin-member", "member-pass-123", false).await;
    for uri in [
        "/api/v1/plugin-ui-contract",
        &format!("/api/v1/plugins/{pid}/ui"),
        "/api/v1/plugin-ui-navigation",
        &asset_uri,
    ] {
        let (st_m, _, _) = send_raw(
            &env,
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(header::COOKIE, format!("mc_session={member_cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_m, StatusCode::FORBIDDEN, "member must get 403 on {uri}");
    }

    assert_admin_cookie_assets_and_logout(&env, &admin_cookie, &asset_uri).await;
}

async fn assert_admin_cookie_assets_and_logout(env: &HttpEnv, admin_cookie: &str, asset_uri: &str) {
    // Cookie-only admin works on GET & HEAD with nosniff + private, no-store
    let (st_get, hdrs_get, bytes_get) = send_raw(
        env,
        Request::builder()
            .method("GET")
            .uri(asset_uri)
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_get, StatusCode::OK);
    assert!(
        hdrs_get
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    assert_eq!(
        hdrs_get.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );
    let cc = hdrs_get
        .get(header::CACHE_CONTROL)
        .unwrap()
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(cc.contains("private") && cc.contains("no-store"));
    assert!(String::from_utf8_lossy(&bytes_get).contains("cookieAuthRemote"));

    let (st_head, hdrs_head, bytes_head) = send_raw(
        env,
        Request::builder()
            .method("HEAD")
            .uri(asset_uri)
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_head, StatusCode::OK);
    assert!(bytes_head.is_empty());
    assert!(
        hdrs_head
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("javascript")
    );

    // Public image endpoint remains unauthenticated (404 missing file, never 401)
    let (st_img, _, _) = send_raw(
        env,
        Request::builder()
            .method("GET")
            .uri("/posters/nonexistent.jpg")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_ne!(st_img, StatusCode::UNAUTHORIZED);

    // Logout clears cookie and revokes session immediately
    let (st_out, out_hdrs, _) = send_raw(
        env,
        Request::builder()
            .method("POST")
            .uri("/api/v1/auth/logout")
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_out, StatusCode::OK);
    assert!(
        out_hdrs
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let (st_after, _, _) = send_raw(
        env,
        Request::builder()
            .method("GET")
            .uri(asset_uri)
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_after, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn trust_prepare_grant_revoke_cas_navigation_and_builtins() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(502);
    env.pin_trust_key(&key, "k-502").await;
    let pid = "org.example.trustflow";
    let pkg = build_ui_package(
        pid,
        "1.0.0",
        "trustFlowRemote",
        &["read:sites"],
        &default_surfaces(),
        &[],
    );
    let hash = sha256_hex(&pkg);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-tf").await;

    let (admin_cookie, _) = login_cookie(&env, "admin", ADMIN_PASS, false).await;

    // Untrusted -> descriptor ui_trusted=false, asset=403, navigation excludes it
    let (st_d0, _, b_d0) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let d0 = assert_ok(st_d0, &b_d0, StatusCode::OK);
    assert_eq!(d0["ui_trusted"], false);
    assert_eq!(d0["trust"]["status"], "untrusted");
    let entry_url = d0["entry_url"].as_str().unwrap().to_string();
    let (st_untrusted, _, _) = send_raw(
        &env,
        Request::builder()
            .method("GET")
            .uri(&entry_url)
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_untrusted, StatusCode::FORBIDDEN);

    // CAS & reauth checks on grant
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
    let reauth_wrong = env.reauth("plugin:upload").await;
    let (st_428, _, b_428) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("idempotency-key", "g-428")],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": hash,
                "consent_token": prep["consent_token"],
                "reauth_token": reauth_wrong,
            })),
        ))
        .await;
    assert_err(
        st_428,
        &b_428,
        StatusCode::PRECONDITION_REQUIRED,
        "plugin.precondition_required",
    );

    let (st_wrong_purpose, _, b_wp) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "g-wp")],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": hash,
                "consent_token": prep["consent_token"],
                "reauth_token": reauth_wrong,
            })),
        ))
        .await;
    assert_eq!(st_wrong_purpose, StatusCode::FORBIDDEN);
    assert_eq!(b_wp["ok"], false);

    // Grant succeeds -> navigation includes page surface (not config), assets succeed
    let granted = grant_ui_trust_via_http(&env, pid, Some(&hash)).await;
    assert_eq!(granted["ui_trusted"], true);
    assert_eq!(granted["revision"], 1);

    assert_nav_builtins_and_revoke(&env, pid, &hash, &entry_url, &admin_cookie).await;
}

async fn assert_nav_builtins_and_revoke(
    env: &HttpEnv,
    pid: &str,
    hash: &str,
    entry_url: &str,
    admin_cookie: &str,
) {
    plugin_http_support::plugins_fixture::install_cm_checkin(env, true, false, false, true).await;
    plugin_http_support::plugins_fixture::install_cm_boost(env, true, false, true).await;

    let (st_nav, _, b_nav) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-ui-navigation",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let nav = assert_ok(st_nav, &b_nav, StatusCode::OK);
    let items = nav.as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|it| it["plugin_id"] == pid && it["kind"] == "page")
    );
    assert!(
        !items
            .iter()
            .any(|it| it["plugin_id"] == pid && it["kind"] == "config")
    );
    let page_nav = items
        .iter()
        .find(|it| it["plugin_id"] == pid && it["kind"] == "page")
        .unwrap();
    assert_eq!(
        page_nav["location"],
        format!("/settings/plugins/{pid}?tab=page&view=main")
    );
    let checkin_app_nav = items
        .iter()
        .find(|it| it["plugin_id"] == "cm.checkin" && it["kind"] == "app")
        .unwrap();
    assert_eq!(checkin_app_nav["location"], "/plugin-apps/cm.checkin/app");

    // Installed official checkin & boost expose 4 surfaces once trusted
    for official in ["cm.checkin", "cm.boost"] {
        let (st_b, _, b_b) = env
            .send(req_json(
                "GET",
                &format!("/api/v1/plugins/{official}/ui"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let db = assert_ok(st_b, &b_b, StatusCode::OK);
        assert_eq!(db["ui_trusted"], true);
        let kinds: std::collections::BTreeSet<_> = db["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["kind"].as_str())
            .collect();
        assert_eq!(
            kinds,
            ["app", "config", "dashboard", "page"].into_iter().collect()
        );
        let (st_ba, _, _) = send_raw(
            env,
            Request::builder()
                .method("GET")
                .uri(db["entry_url"].as_str().unwrap())
                .header(header::COOKIE, format!("mc_session={admin_cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_ba, StatusCode::OK);
    }

    // Revoke blocks assets & hides nav while plugin remains enabled + writes audit
    let (st_rev, _, b_rev) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/revoke"),
            Some(TOKEN),
            &[("if-match", "\"1\""), ("idempotency-key", "rev-1")],
            Some(json!({ "package_hash": hash })),
        ))
        .await;
    let revoked = assert_ok(st_rev, &b_rev, StatusCode::OK);
    assert_eq!(revoked["status"], "revoked");
    let (st_after_rev, _, _) = send_raw(
        env,
        Request::builder()
            .method("GET")
            .uri(entry_url)
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_after_rev, StatusCode::FORBIDDEN);
    let (st_aud, _, b_aud) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/audit"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_aud, StatusCode::OK);
    let aud_str = b_aud.to_string();
    assert!(aud_str.contains("ui_trust.grant") && aud_str.contains("ui_trust.revoke"));
}

#[tokio::test]
async fn traversal_undeclared_stale_hash_upgrade_reset_disabled_remote_conflict_and_422_limit() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let key = TestSigningKey::generate(503);
    env.pin_trust_key(&key, "k-503").await;
    let (admin_cookie, _) = login_cookie(&env, "admin", ADMIN_PASS, false).await;

    let pid = "org.example.hardenedui";
    let pkg1 = build_ui_package(
        pid,
        "1.0.0",
        "hardenedRemote",
        &["read:sites"],
        &default_surfaces(),
        &[],
    );
    let h1 = sha256_hex(&pkg1);
    env.install_pkg(&pkg1, Some(&key.sign_package(&pkg1)), json!([]))
        .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-h1").await;
    grant_ui_trust_via_http(&env, pid, Some(&h1)).await;

    // Traversal & undeclared paths & wrong hash blocked before disk read
    for (bad_path, expected) in [
        (
            format!("/api/v1/plugin-ui-assets/{pid}/{h1}/%2e%2e/plugin.toml"),
            &[StatusCode::BAD_REQUEST, StatusCode::FORBIDDEN][..],
        ),
        (
            format!("/api/v1/plugin-ui-assets/{pid}/{h1}/ui/assets/undeclared.js"),
            &[StatusCode::NOT_FOUND, StatusCode::FORBIDDEN][..],
        ),
        (
            format!("/api/v1/plugin-ui-assets/{pid}/{h1}/ui/asset-manifest.json"),
            &[StatusCode::NOT_FOUND, StatusCode::FORBIDDEN][..],
        ),
        (
            format!(
                "/api/v1/plugin-ui-assets/{pid}/{}/ui/assets/remoteEntry.js",
                "f".repeat(64)
            ),
            &[StatusCode::NOT_FOUND, StatusCode::CONFLICT][..],
        ),
    ] {
        let (st, _, _) = send_raw(
            &env,
            Request::builder()
                .method("GET")
                .uri(&bad_path)
                .header(header::COOKIE, format!("mc_session={admin_cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(
            expected.contains(&st),
            "unexpected status {st} for {bad_path}"
        );
    }

    // Disabling marks page unmountable, config editable, and keeps Config bundle assets readable
    let rev = env.get_plugin_rev(pid).await;
    env.disable_plugin(pid, rev, "dis-h1").await;
    let (st_ui_dis, _, b_ui_dis) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let ui_dis = assert_ok(st_ui_dis, &b_ui_dis, StatusCode::OK);
    let surfaces = ui_dis["surfaces"].as_array().unwrap();
    let cfg = surfaces.iter().find(|s| s["kind"] == "config").unwrap();
    let pg = surfaces.iter().find(|s| s["kind"] == "page").unwrap();
    assert_eq!(cfg["editable"], true);
    assert_eq!(pg["mountable"], false);
    let (st_asset_dis, _, _) = send_raw(
        &env,
        Request::builder()
            .method("GET")
            .uri(ui_dis["entry_url"].as_str().unwrap())
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st_asset_dis, StatusCode::OK);

    assert_dev_reload_and_asset_limit_422(&env, &key).await;
}

async fn assert_dev_reload_and_asset_limit_422(env: &HttpEnv, key: &TestSigningKey) {
    let dev_pid = "org.example.devui";
    let dev_v1 = build_ui_package(
        dev_pid,
        "1.0.0",
        "devUiRemote",
        &["read:sites"],
        &default_surfaces(),
        &[],
    );
    let (st_d1, _, _) = env
        .send(
            Request::builder()
                .method("POST")
                .uri("/api/v1/plugins/dev")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(dev_v1))
                .unwrap(),
        )
        .await;
    assert_eq!(st_d1, StatusCode::CREATED);
    let rev_d1 = env.get_plugin_rev(dev_pid).await;
    env.enable_plugin(dev_pid, rev_d1, "en-d1").await;
    grant_ui_trust_via_http(env, dev_pid, None).await;
    let rev_d1 = env.get_plugin_rev(dev_pid).await;
    env.disable_plugin(dev_pid, rev_d1, "dis-d1").await;
    let dev_v2 = build_ui_package(
        dev_pid,
        "1.0.1",
        "devUiRemote",
        &["read:sites"],
        &default_surfaces(),
        &[],
    );
    let (st_d2, _, _) = env
        .send(
            Request::builder()
                .method("POST")
                .uri("/api/v1/plugins/dev")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(dev_v2))
                .unwrap(),
        )
        .await;
    assert_eq!(st_d2, StatusCode::OK);
    let (st_ui_d2, _, b_ui_d2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{dev_pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(
        assert_ok(st_ui_d2, &b_ui_d2, StatusCode::OK)["ui_trusted"],
        false
    );

    let tiny = b"export const x = 1;\n";
    let paths: Vec<String> = (0..1023).map(|i| format!("ui/assets/f{i}.js")).collect();
    let extras: Vec<(&str, &[u8], &str)> = paths
        .iter()
        .map(|p| (p.as_str(), tiny.as_slice(), "application/javascript"))
        .collect();
    let big_zip = build_ui_package(
        "org.example.toomanyassets",
        "1.0.0",
        "tooManyAssets",
        &["read:sites"],
        &default_surfaces(),
        &extras,
    );
    let reauth = env.reauth("plugin:upload").await;
    let uid = Uuid::new_v4().to_string();
    let sig = key.sign_package(&big_zip);
    let req = plugin_http_support::req_multipart(
        "/api/v1/plugin-uploads",
        Some(TOKEN),
        &[("if-match", "\"0\""), ("idempotency-key", "up-limit-422")],
        "----LimitBoundary",
        &[
            ("upload_id", None, uid.as_bytes()),
            ("file", Some("plugin.cmplugin"), &big_zip),
            ("signature", Some("plugin.minisig"), sig.as_bytes()),
            ("reauth_token", None, reauth.as_bytes()),
        ],
    );
    let (st_lim, _, b_lim) = env.send(req).await;
    assert_eq!(
        st_lim,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422, got {b_lim}"
    );
}
