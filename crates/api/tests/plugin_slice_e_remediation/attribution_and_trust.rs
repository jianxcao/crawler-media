use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use domain::{
    DownloaderId, Media, MediaId, MediaKind, SiteId, SubscribeId, User, UserId, UserRole,
};
use serde_json::json;

use super::support::plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok,
    pkg::TestSigningKey,
    plugins_fixture::{install_cm_boost, install_cm_checkin},
    req_json,
};
use super::support::poll_op;
use super::{
    build_ui_pkg, grant_ui_trust_http, login_admin_cookie, publish_real_library_movie, send_raw,
    sha256_hex,
};

#[tokio::test]
async fn ui_query_media_get_enriches_routing_fields_and_enforces_handle_and_search_ownership() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(702);
    env.pin_trust_key(&key, "k-702").await;
    let site_id = env.insert_site("PT-Ownership");
    let lib_dir = env._dir.path().join("movies_lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    let movie_path = lib_dir.join("Dune.mkv");
    std::fs::write(&movie_path, b"dune").unwrap();
    let (lib_id, lib_media_id) =
        publish_real_library_movie(env.state.store().as_ref(), &lib_dir, &movie_path);

    let catalog_media_id = MediaId::new();
    env.state
        .store()
        .insert_media(&Media {
            id: catalog_media_id,
            kind: MediaKind::Tv,
            title: "Catalog Show".into(),
            year: Some(2024),
            original_title: None,
            tmdb_id: Some("998877".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();

    let pid_full = "org.example.uifull";
    let pkg_full = build_ui_pkg(
        pid_full,
        "uiFullRemote",
        &[
            "read:sites",
            "read:catalog",
            "read:library",
            "command:indexer.search",
        ],
        false,
    );
    env.install_pkg(
        &pkg_full,
        Some(&key.sign_package(&pkg_full)),
        json!([{ "capability": "command:indexer.search", "scope": "Global", "sensitive": false }]),
    )
    .await;
    let rev_f = env.get_plugin_rev(pid_full).await;
    env.enable_plugin(pid_full, rev_f, "en-full").await;
    grant_ui_trust_http(&env, pid_full).await;

    let (st_m1, _, b_m1) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_full}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "media.get", "input": { "media_id": lib_media_id } })),
        ))
        .await;
    let m1 = assert_ok(st_m1, &b_m1, StatusCode::OK);
    assert_eq!(m1["tmdb_id"], "438631");
    assert_eq!(m1["douban_id"], "26954859");
    assert_eq!(m1["library_id"], lib_id);
    assert_eq!(m1["revision"], "1");

    let (st_m2, _, b_m2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_full}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "media.get", "input": { "id": catalog_media_id } })),
        ))
        .await;
    let m2 = assert_ok(st_m2, &b_m2, StatusCode::OK);
    assert_eq!(m2["tmdb_id"], "998877");
    assert!(m2["douban_id"].is_null() && m2["library_id"].is_null());
    assert_eq!(m2["revision"], "1");

    assert_handle_caps_and_search_ownership(&env, &key, pid_full, site_id, &lib_id, lib_media_id)
        .await;
}

async fn assert_handle_caps_and_search_ownership(
    env: &HttpEnv,
    key: &TestSigningKey,
    pid_full: &str,
    site_id: SiteId,
    lib_id: &str,
    media_id: MediaId,
) {
    let pid_sites = "org.example.uisitesonly";
    let pkg_sites = build_ui_pkg(pid_sites, "uiSitesOnlyRemote", &["read:sites"], false);
    env.install_pkg(&pkg_sites, Some(&key.sign_package(&pkg_sites)), json!([]))
        .await;
    let rev_s = env.get_plugin_rev(pid_sites).await;
    env.enable_plugin(pid_sites, rev_s, "en-so").await;
    grant_ui_trust_http(env, pid_sites).await;

    let (st_ok, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_sites}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "handles.resolve", "input": { "kind": "site", "id": site_id } })),
        ))
        .await;
    assert_eq!(st_ok, StatusCode::OK);

    for (kind, id) in [
        ("library", lib_id.to_string()),
        ("media", media_id.to_string()),
        ("subscribe", SubscribeId::new().to_string()),
        ("downloader", DownloaderId::new().to_string()),
        ("owned_download", "task-123".to_string()),
        ("download_task", "op-123".to_string()),
    ] {
        let (st_den, _, b_den) = env
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid_sites}/ui/query"),
                Some(TOKEN),
                &[],
                Some(json!({ "name": "handles.resolve", "input": { "kind": kind, "id": id } })),
            ))
            .await;
        assert_err(
            st_den,
            &b_den,
            StatusCode::FORBIDDEN,
            "plugin.capability_denied",
        );
    }

    let rev_f = env.get_plugin_rev(pid_full).await;
    let (st_srch, _, b_srch) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_full}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_f}\"")),
                ("idempotency-key", "srch-owner-a"),
            ],
            Some(json!({
                "kind": "indexer.search",
                "payload": { "site_id": site_id.to_string(), "keyword": "Dune" }
            })),
        ))
        .await;
    let op_id = assert_ok(st_srch, &b_srch, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    poll_op(env, &op_id).await;

    let user_b = User {
        id: UserId::new(),
        login: "admin-b".into(),
        enabled: true,
        role: UserRole::Admin,
    };
    env.state
        .store()
        .insert_user_with_password(&user_b, "admin2-pass-123456")
        .unwrap();
    env.state
        .store()
        .set_user_token(user_b.id, "admin-b-slice-e-token")
        .unwrap();

    let (st_b, _, b_b) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_full}/ui/query"),
            Some("admin-b-slice-e-token"),
            &[],
            Some(json!({ "name": "indexer.cached", "input": { "search_operation_id": op_id } })),
        ))
        .await;
    assert_err(st_b, &b_b, StatusCode::FORBIDDEN, "plugin.forbidden");

    let (st_a, _, b_a) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_full}/ui/query"),
            Some(TOKEN),
            &[],
            Some(json!({ "name": "indexer.cached", "input": { "search_operation_id": op_id } })),
        ))
        .await;
    assert_ok(st_a, &b_a, StatusCode::OK);
}

#[tokio::test]
async fn sdk_read_endpoints_enforce_ui_attribution_and_external_assets_verify_sha256() {
    let env = HttpEnv::new().await;
    install_cm_checkin(&env, true, true, false, true).await;
    install_cm_boost(&env, true, false, true).await;
    let key = TestSigningKey::generate(705);
    env.pin_trust_key(&key, "k-705").await;
    let site_id = env.insert_site("PT-AttrRead");
    let admin_cookie = login_admin_cookie(&env).await;

    let pid = "org.example.attrread";
    let pkg = build_ui_pkg(pid, "attrReadRemote", &["read:sites"], true);
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(
        &pkg,
        Some(&key.sign_package(&pkg)),
        json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
    )
    .await;
    let rev = env.get_plugin_rev(pid).await;
    let en_op = env.enable_plugin(pid, rev, "en-ar").await;
    let own_op_id = en_op["operation_id"].as_str().unwrap().to_string();
    grant_ui_trust_http(&env, pid).await;

    for uri in [
        format!("/api/v1/plugins/{pid}/tasks"),
        format!("/api/v1/plugins/{pid}/tasks/sync/receipts"),
        format!("/api/v1/plugins/{pid}/browser-availability?site_id={site_id}&purpose=search"),
        format!("/api/v1/plugin-operations/{own_op_id}"),
    ] {
        let (st, _, b) = env
            .send(req_json(
                "GET",
                &uri,
                Some(TOKEN),
                &[
                    ("x-plugin-package", &pkg_hash),
                    ("x-plugin-generation", "999"),
                ],
                None,
            ))
            .await;
        assert_err(st, &b, StatusCode::CONFLICT, "plugin.stale_generation");
    }

    let (st_rc_den, _, b_rc_den) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/tasks/sync/receipts"),
            Some(TOKEN),
            &[
                ("x-plugin-package", &pkg_hash),
                ("x-plugin-generation", "1"),
            ],
            None,
        ))
        .await;
    assert_err(
        st_rc_den,
        &b_rc_den,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );

    for (bpid, tid) in [("cm.checkin", "check-in"), ("cm.boost", "traffic-cycle")] {
        let bp = env.state.store().get_plugin(bpid).unwrap().unwrap();
        let (st_brc, _, b_brc) = env
            .send(req_json(
                "GET",
                &format!("/api/v1/plugins/{bpid}/tasks/{tid}/receipts"),
                Some(TOKEN),
                &[
                    ("x-plugin-package", &bp.package_hash),
                    ("x-plugin-generation", &bp.generation.to_string()),
                ],
                None,
            ))
            .await;
        assert_eq!(st_brc, StatusCode::OK, "{bpid} receipts failed: {b_brc}");
    }

    assert_get_op_cross_plugin_and_corrupted_asset(&env, pid, &pkg_hash, &own_op_id, &admin_cookie)
        .await;
}

async fn assert_get_op_cross_plugin_and_corrupted_asset(
    env: &HttpEnv,
    pid: &str,
    pkg_hash: &str,
    own_op_id: &str,
    admin_cookie: &str,
) {
    let (st_own, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{own_op_id}"),
            Some(TOKEN),
            &[("x-plugin-package", pkg_hash), ("x-plugin-generation", "1")],
            None,
        ))
        .await;
    assert_eq!(st_own, StatusCode::OK);

    let boost_rev = env.get_plugin_rev("cm.boost").await;
    let boost_dis = env
        .disable_plugin("cm.boost", boost_rev, "dis-boost-op")
        .await;
    let foreign_op_id = boost_dis["operation_id"].as_str().unwrap();
    let (st_for, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{foreign_op_id}"),
            Some(TOKEN),
            &[("x-plugin-package", pkg_hash), ("x-plugin-generation", "1")],
            None,
        ))
        .await;
    assert!(matches!(
        st_for,
        StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));

    let asset_uri = format!("/api/v1/plugin-ui-assets/{pid}/{pkg_hash}/ui/assets/remoteEntry.js");
    let get_asset = || {
        Request::builder()
            .method("GET")
            .uri(&asset_uri)
            .header(header::COOKIE, format!("mc_session={admin_cookie}"))
            .body(Body::empty())
            .unwrap()
    };
    let (st_ok, _, _) = send_raw(env, get_asset()).await;
    assert_eq!(st_ok, StatusCode::OK);

    let ver = env
        .state
        .store()
        .get_plugin_version(pid, pkg_hash)
        .unwrap()
        .unwrap();
    let pkg_root = if Path::new(&ver.artifact_path).is_absolute() {
        std::path::PathBuf::from(&ver.artifact_path)
    } else {
        env.state.store().data_dir().join(&ver.artifact_path)
    };
    let entry_file = pkg_root.join("ui/assets/remoteEntry.js");
    let orig_entry = std::fs::read(&entry_file).unwrap();
    std::fs::write(&entry_file, b"export const tampered = true;\n").unwrap();

    let (st_t1, _, b_t1) = send_raw(env, get_asset()).await;
    assert_err(
        st_t1,
        &serde_json::from_slice(&b_t1).unwrap(),
        StatusCode::FORBIDDEN,
        "plugin.ui_asset_corrupted",
    );

    std::fs::write(&entry_file, &orig_entry).unwrap();
    let bad_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{ "path": "ui/assets/remoteEntry.js", "sha256": sha256_hex(&orig_entry), "size": orig_entry.len(), "content_type": "application/javascript" }],
        "tampered": true
    }))
    .unwrap();
    std::fs::write(pkg_root.join("ui/asset-manifest.json"), bad_manifest).unwrap();

    let (st_t2, _, b_t2) = send_raw(env, get_asset()).await;
    assert_err(
        st_t2,
        &serde_json::from_slice(&b_t2).unwrap(),
        StatusCode::FORBIDDEN,
        "plugin.ui_asset_corrupted",
    );
}

#[tokio::test]
async fn ui_trust_grant_and_revoke_replay_cleanly_and_validate_idempotency() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(706);
    env.pin_trust_key(&key, "k-706").await;
    let pid = "org.example.trustreplay";
    let pkg = build_ui_pkg(pid, "trustReplayRemote", &["read:sites"], false);
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;

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
    let grant_body = json!({
        "preparation_id": prep["preparation_id"],
        "package_hash": pkg_hash,
        "consent_token": prep["consent_token"],
        "reauth_token": reauth,
    });

    let long_key = "k".repeat(300);
    let (st_bad_idem, _, b_bad_idem) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", &long_key)],
            Some(grant_body.clone()),
        ))
        .await;
    assert_eq!(st_bad_idem, StatusCode::BAD_REQUEST, "got {b_bad_idem}");

    let make_grant_req = || {
        req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "uit-replay-g1")],
            Some(grant_body.clone()),
        )
    };
    let ((st_g1, _, b_g1), (st_g2, _, b_g2)) =
        tokio::join!(env.send(make_grant_req()), env.send(make_grant_req()));
    assert_eq!(assert_ok(st_g1, &b_g1, StatusCode::OK)["revision"], 1);
    assert_eq!(assert_ok(st_g2, &b_g2, StatusCode::OK)["revision"], 1);
    let (st_g3, _, b_g3) = env.send(make_grant_req()).await;
    assert_eq!(assert_ok(st_g3, &b_g3, StatusCode::OK)["revision"], 1);

    let (st_conf, _, b_conf) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "uit-replay-g1")],
            Some(json!({ "consent_token": "x", "reauth_token": "y" })),
        ))
        .await;
    assert_err(
        st_conf,
        &b_conf,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );

    let make_rev_req = || {
        req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/revoke"),
            Some(TOKEN),
            &[("if-match", "\"1\""), ("idempotency-key", "uit-replay-r1")],
            Some(json!({ "package_hash": pkg_hash })),
        )
    };
    let ((st_r1, _, b_r1), (st_r2, _, b_r2)) =
        tokio::join!(env.send(make_rev_req()), env.send(make_rev_req()));
    assert_eq!(assert_ok(st_r1, &b_r1, StatusCode::OK)["revision"], 2);
    assert_eq!(assert_ok(st_r2, &b_r2, StatusCode::OK)["revision"], 2);

    let audits = env
        .state
        .store()
        .list_plugin_audit_events(pid, None, None, None, Some(50))
        .unwrap()
        .items;
    assert_eq!(
        audits
            .iter()
            .filter(|a| a.action == "ui_trust.grant")
            .count(),
        1
    );
    assert_eq!(
        audits
            .iter()
            .filter(|a| a.action == "ui_trust.revoke")
            .count(),
        1
    );
}
