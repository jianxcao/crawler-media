use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, pkg::TestSigningKey, req_json,
};
use crate::{build_ui_package, sha256_hex};

async fn login_admin_cookie(env: &HttpEnv) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "username": "admin", "password": ADMIN_PASS }).to_string(),
        ))
        .unwrap();
    let resp = env.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    set_cookie.split(';').next().unwrap().to_string()
}

fn resolve_pkg_root(env: &HttpEnv, pid: &str, pkg_hash: &str) -> PathBuf {
    let ver = env
        .state
        .store()
        .get_plugin_version(pid, pkg_hash)
        .unwrap()
        .unwrap();
    let art = Path::new(&ver.artifact_path);
    if art.is_absolute() {
        art.to_path_buf()
    } else {
        env.state.store().data_dir().join(art)
    }
}

#[cfg(unix)]
#[tokio::test]
async fn slice_e_r5_item4_ui_asset_manifest_enforces_containment_no_symlinks_and_metadata_sha256() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(541);
    env.pin_trust_key(&key, "k-541").await;

    let pid = "org.example.r5ui";
    let pkg = build_ui_package(pid, "r5UiRemote", &["read:sites"]);
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;

    let pkg_root = resolve_pkg_root(&env, pid, &pkg_hash);
    let manifest_path = pkg_root.join("ui/asset-manifest.json");
    let orig_manifest = std::fs::read(&manifest_path).unwrap();

    // 1) Tamper with ui/asset-manifest.json BEFORE prepare_ui_trust & get_plugin_ui_descriptor!
    let tampered = serde_json::to_vec(&json!({
        "version": 1,
        "files": [],
        "evil": true
    }))
    .unwrap();
    std::fs::write(&manifest_path, &tampered).unwrap();

    let (st_prep_bad, _, b_prep_bad) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_err(
        st_prep_bad,
        &b_prep_bad,
        StatusCode::FORBIDDEN,
        "plugin.ui_asset_corrupted",
    );

    let (st_desc_bad, _, b_desc_bad) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_desc_bad,
        &b_desc_bad,
        StatusCode::FORBIDDEN,
        "plugin.ui_asset_corrupted",
    );

    // 2) Restore valid manifest, grant trust, then swap ui/asset-manifest.json for a symlink!
    std::fs::write(&manifest_path, &orig_manifest).unwrap();
    grant_trust_for_pkg(&env, pid, &pkg_hash).await;
    assert_symlink_manifest_rejected(&env, pid, &pkg_hash, &pkg_root, &orig_manifest).await;
}

async fn grant_trust_for_pkg(env: &HttpEnv, pid: &str, pkg_hash: &str) {
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep: Value = assert_ok(st_p, &b_p, StatusCode::OK);
    let reauth = env.reauth("plugin-ui-trust").await;
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "r5-ui4-grant")],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": pkg_hash,
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK);
}

#[cfg(unix)]
async fn assert_symlink_manifest_rejected(
    env: &HttpEnv,
    pid: &str,
    pkg_hash: &str,
    pkg_root: &Path,
    orig_manifest: &[u8],
) {
    let outside_manifest = pkg_root.parent().unwrap().join("outside-manifest.json");
    std::fs::write(&outside_manifest, orig_manifest).unwrap();
    let manifest_path = pkg_root.join("ui/asset-manifest.json");
    std::fs::remove_file(&manifest_path).unwrap();
    std::os::unix::fs::symlink(&outside_manifest, &manifest_path).unwrap();

    let (st_desc_sym, _, b_desc_sym) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/ui"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_desc_sym,
        &b_desc_sym,
        StatusCode::FORBIDDEN,
        "plugin.invalid_asset_path",
    );

    let (st_prep_sym, _, b_prep_sym) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_err(
        st_prep_sym,
        &b_prep_sym,
        StatusCode::FORBIDDEN,
        "plugin.invalid_asset_path",
    );

    let cookie = login_admin_cookie(env).await;
    let (st_asset_sym, _, b_asset_sym) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-ui-assets/{pid}/{pkg_hash}/ui/assets/remoteEntry.js"),
            None,
            &[("cookie", &cookie)],
            None,
        ))
        .await;
    assert_err(
        st_asset_sym,
        &b_asset_sym,
        StatusCode::FORBIDDEN,
        "plugin.invalid_asset_path",
    );
}
