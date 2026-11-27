#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use axum::http::StatusCode;
use plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok, etag, location, pkg::build_task_package, req_json,
};
use serde_json::json;

#[tokio::test]
async fn lifecycle_enable_idempotency_replay_and_conflict() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.life1", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    let if_match = format!("\"{}\"", detail["revision"].as_i64().unwrap());

    let (st_e1, h_e1, b_e1) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.life1/enable",
            Some(TOKEN),
            &[("if-match", &if_match), ("idempotency-key", "life-en-1")],
            Some(json!({})),
        ))
        .await;
    let op_e1 = assert_ok(st_e1, &b_e1, StatusCode::ACCEPTED);
    assert_eq!(
        location(&h_e1),
        format!(
            "/api/v1/plugin-operations/{}",
            op_e1["operation_id"].as_str().unwrap()
        )
    );
    assert_eq!(etag(&h_e1), format!("\"{}\"", op_e1["revision"]));

    // Replay with original If-Match after revision advanced -> replays original 202.
    let (st_e2, _, b_e2) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.life1/enable",
            Some(TOKEN),
            &[("if-match", &if_match), ("idempotency-key", "life-en-1")],
            Some(json!({})),
        ))
        .await;
    assert_eq!(
        assert_ok(st_e2, &b_e2, StatusCode::ACCEPTED)["operation_id"],
        op_e1["operation_id"]
    );

    let (st_ic, _, b_ic) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.life1/disable",
            Some(TOKEN),
            &[("if-match", &if_match), ("idempotency-key", "life-en-1")],
            Some(json!({})),
        ))
        .await;
    assert_err(
        st_ic,
        &b_ic,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );

    let (st_miss, _, b_miss) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.life1/disable",
            Some(TOKEN),
            &[("if-match", &if_match)],
            Some(json!({})),
        ))
        .await;
    assert_err(
        st_miss,
        &b_miss,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );

    let long_key = "k".repeat(257);
    let (st_long, _, b_long) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.life1/disable",
            Some(TOKEN),
            &[("if-match", &if_match), ("idempotency-key", &long_key)],
            Some(json!({})),
        ))
        .await;
    assert_err(
        st_long,
        &b_long,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
}

#[tokio::test]
async fn lifecycle_disable_and_uninstall_routes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.life2", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(
        "org.example.life2",
        detail["revision"].as_i64().unwrap(),
        "en-l2",
    )
    .await;

    let (st_g, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.life2",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev_en = assert_ok(st_g, &b_g, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    env.disable_plugin("org.example.life2", rev_en, "dis-l2")
        .await;

    let (st_g2, _, b_g2) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.life2",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev_dis = assert_ok(st_g2, &b_g2, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    let (st_un, h_un, b_un) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.life2/uninstall",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", "un-l2"),
            ],
            Some(json!({ "remove_private_data": true })),
        ))
        .await;
    let op_un = assert_ok(st_un, &b_un, StatusCode::ACCEPTED);
    assert!(location(&h_un).starts_with("/api/v1/plugin-operations/"));
    assert_eq!(op_un["state"], "succeeded");
}

#[tokio::test]
async fn upgrade_preserve_auto_drains_enabled_plugin() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg1 = build_task_package("org.example.upgp", "1.0.0", 1, false);
    let d1 = env.install_pkg(&pkg1, None, json!([])).await;
    env.enable_plugin("org.example.upgp", d1["revision"].as_i64().unwrap(), "en-1")
        .await;

    let (st_g, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.upgp",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev_en = assert_ok(st_g, &b_g, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.upgp/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let pkg2 = build_task_package("org.example.upgp", "1.1.0", 1, false);
    let up2 = env.upload_pkg(&pkg2, None, "up-v2").await;
    let (st_p2, _, b_p2) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.upgp/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev_en}\"")), ("idempotency-key", "upg-p-2")], Some(json!({
            "version": "1.1.0", "upload_id": up2["upload_id"],
            "target_config": { "schema_revision": srev, "values": { "mode": "v2" } }, "state_policy": "preserve"
        }))))
        .await;
    let prep = assert_ok(st_p2, &b_p2, StatusCode::ACCEPTED)["result"].clone();
    let (st_u2, h_u2, b_u2) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.upgp/upgrade", Some(TOKEN), &[("if-match", &format!("\"{rev_en}\"")), ("idempotency-key", "upg-e-2")], Some(json!({
            "preparation_id": prep["preparation_id"], "consent_token": prep["consent_token"], "grants": [],
            "state_policy": "preserve", "target_config_digest": prep["config_diff"]["target_config_digest"]
        }))))
        .await;
    let upg_op = assert_ok(st_u2, &b_u2, StatusCode::ACCEPTED);
    assert!(location(&h_u2).starts_with("/api/v1/plugin-operations/"));
    assert_eq!(upg_op["state"], "succeeded");
}

#[tokio::test]
async fn upgrade_preserve_rejects_state_schema_mismatch() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg1 = build_task_package("org.example.upgm", "1.0.0", 1, false);
    let d1 = env.install_pkg(&pkg1, None, json!([])).await;
    let rev1 = d1["revision"].as_i64().unwrap();
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.upgm/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let pkg2 = build_task_package("org.example.upgm", "2.0.0", 2, false);
    let up2 = env.upload_pkg(&pkg2, None, "up-mm").await;
    let (st_bad, _, b_bad) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.upgm/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "upg-p-bad")], Some(json!({
            "version": "2.0.0", "upload_id": up2["upload_id"],
            "target_config": { "schema_revision": srev, "values": { "mode": "v2" } }, "state_policy": "preserve"
        }))))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::CONFLICT,
        "plugin.state_schema_mismatch",
    );
}

#[tokio::test]
async fn upgrade_reset_and_rollback_with_reauth_succeed() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg1 = build_task_package("org.example.rb", "1.0.0", 1, false);
    let d1 = env.install_pkg(&pkg1, None, json!([])).await;
    let hash1 = d1["package_hash"].as_str().unwrap().to_string();
    let rev1 = d1["revision"].as_i64().unwrap();
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.rb/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let pkg2 = build_task_package("org.example.rb", "2.0.0", 2, false);
    let up2 = env.upload_pkg(&pkg2, None, "up-rb-2").await;
    let (st_p2, _, b_p2) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.rb/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "rb-up-p")], Some(json!({
            "version": "2.0.0", "upload_id": up2["upload_id"], "target_config": { "schema_revision": srev, "values": { "mode": "v2" } }, "state_policy": "reset"
        }))))
        .await;
    let prep2 = assert_ok(st_p2, &b_p2, StatusCode::ACCEPTED)["result"].clone();
    let reauth_up = env.reauth("plugin:upgrade").await;
    let (st_u2, _, b_u2) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.rb/upgrade", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "rb-up-e")], Some(json!({
            "preparation_id": prep2["preparation_id"], "consent_token": prep2["consent_token"], "grants": [],
            "state_policy": "reset", "target_config_digest": prep2["config_diff"]["target_config_digest"], "reauth_token": reauth_up
        }))))
        .await;
    assert_eq!(
        assert_ok(st_u2, &b_u2, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );

    let (st_g, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.rb",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev2 = assert_ok(st_g, &b_g, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    let (st_prb, _, b_prb) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.rb/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev2}\"")), ("idempotency-key", "rb-prep")], Some(json!({
            "version": "1.0.0", "package_hash": hash1, "target_config": { "schema_revision": srev, "values": { "mode": "ok" } }, "state_policy": "reset"
        }))))
        .await;
    let prep_rb = assert_ok(st_prb, &b_prb, StatusCode::ACCEPTED)["result"].clone();
    let reauth_rb = env.reauth("plugin:rollback").await;
    let (st_rb, h_rb, b_rb) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.rb/rollback", Some(TOKEN), &[("if-match", &format!("\"{rev2}\"")), ("idempotency-key", "rb-exec")], Some(json!({
            "package_hash": hash1, "preparation_id": prep_rb["preparation_id"], "consent_token": prep_rb["consent_token"], "grants": [],
            "state_policy": "reset", "target_config_digest": prep_rb["config_diff"]["target_config_digest"], "reauth_token": reauth_rb
        }))))
        .await;
    assert_eq!(
        assert_ok(st_rb, &b_rb, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
    assert!(location(&h_rb).starts_with("/api/v1/plugin-operations/"));
}

#[tokio::test]
async fn state_reset_prepare_and_execute_with_http_security_scopes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("Reset Scope Site");
    let pkg = build_task_package("org.example.srhttp", "1.0.0", 1, false);
    let d = env.install_pkg(&pkg, None, json!([])).await;
    let rev = d["revision"].as_i64().unwrap();

    let (st_bad, _, b_bad) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.srhttp/state/reset/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "security_scopes": [{ "site_id": uuid::Uuid::new_v4() }] })),
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.scope_invalid",
    );

    let scopes = json!(["global", { "site_id": site_id }]);
    let (st_pr, _, b_pr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.srhttp/state/reset/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "security_scopes": scopes })),
        ))
        .await;
    let prep = assert_ok(st_pr, &b_pr, StatusCode::OK);
    let reauth = env.reauth("plugin:state-reset").await;
    let (st_ex, h_ex, b_ex) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.srhttp/state/reset", Some(TOKEN), &[("if-match", &format!("\"{rev}\"")), ("idempotency-key", "sr-exec-1")], Some(json!({
            "security_scopes": scopes, "consent_token": prep["consent_token"], "reauth_token": reauth
        }))))
        .await;
    assert_eq!(
        assert_ok(st_ex, &b_ex, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
    assert!(location(&h_ex).starts_with("/api/v1/plugin-operations/"));
}

#[tokio::test]
async fn upgrade_prepare_artifact_missing_sanitizes_filesystem_path() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg1 = build_task_package("org.example.missart", "1.0.0", 1, false);
    let d1 = env.install_pkg(&pkg1, None, json!([])).await;
    let hash1 = d1["package_hash"].as_str().unwrap().to_string();
    let rev1 = d1["revision"].as_i64().unwrap();
    let pkg2 = build_task_package("org.example.missart", "1.1.0", 1, false);
    let up2 = env.upload_pkg(&pkg2, None, "up-miss-2").await;
    let srev = plugin_http_support::pkg::schema_rev(false, None);
    let (st_p, _, b_p) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.missart/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "miss-up-p")], Some(json!({
            "version": "1.1.0", "upload_id": up2["upload_id"], "target_config": { "schema_revision": srev, "values": { "mode": "ok" } }, "state_policy": "preserve"
        }))))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::ACCEPTED)["result"].clone();
    let (st_u, _, b_u) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.missart/upgrade", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "miss-up-e")], Some(json!({
            "preparation_id": prep["preparation_id"], "consent_token": prep["consent_token"], "grants": [],
            "state_policy": "preserve", "target_config_digest": prep["config_diff"]["target_config_digest"]
        }))))
        .await;
    assert_eq!(
        assert_ok(st_u, &b_u, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );

    let data_dir = env.state.store().data_dir().to_path_buf();
    let pkg_dir = data_dir.join("plugins").join("packages").join(&hash1);
    std::fs::remove_dir_all(&pkg_dir).unwrap();
    let rev2 = env.get_plugin_rev("org.example.missart").await;
    let (st_404, _, b_404) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.missart/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev2}\"")), ("idempotency-key", "miss-rb-p")], Some(json!({
            "version": "1.0.0", "package_hash": hash1, "target_config": { "schema_revision": srev, "values": { "mode": "ok" } }, "state_policy": "preserve"
        }))))
        .await;
    let err_obj = assert_err(
        st_404,
        &b_404,
        StatusCode::NOT_FOUND,
        "plugin.artifact_missing",
    );
    let msg = err_obj["message"].as_str().unwrap();
    assert!(
        !msg.contains(&data_dir.display().to_string()) && !msg.contains("/plugins/packages/"),
        "404 leaked internal path: {msg}"
    );
}

#[tokio::test]
async fn idempotency_canonical_json_replays_reordered_keys_and_rejects_changes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg1 = build_task_package("org.example.canon", "1.0.0", 1, false);
    let d1 = env.install_pkg(&pkg1, None, json!([])).await;
    let rev1 = d1["revision"].as_i64().unwrap();
    let srev = plugin_http_support::pkg::schema_rev(false, None);
    let pkg2 = build_task_package("org.example.canon", "1.1.0", 1, false);
    let up2 = env.upload_pkg(&pkg2, None, "canon-up").await;
    let (st_p, _, b_p) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.canon/upgrades/prepare", Some(TOKEN), &[("if-match", &format!("\"{rev1}\"")), ("idempotency-key", "canon-prep")], Some(json!({
            "version": "1.1.0", "upload_id": up2["upload_id"], "target_config": { "schema_revision": srev, "values": { "mode": "ok" } }, "state_policy": "preserve"
        }))))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::ACCEPTED)["result"].clone();
    let prep_id = prep["preparation_id"].as_str().unwrap();
    let consent = prep["consent_token"].as_str().unwrap();
    let digest = prep["config_diff"]["target_config_digest"]
        .as_str()
        .unwrap();

    let raw1 = format!(
        r#"{{"preparation_id":"{prep_id}","consent_token":"{consent}","grants":[],"state_policy":"preserve","target_config_digest":"{digest}"}}"#
    );
    let raw2 = format!(
        r#"{{  "target_config_digest": "{digest}", "grants": [], "state_policy": "preserve", "consent_token": "{consent}", "preparation_id": "{prep_id}"  }}"#
    );
    let raw_diff = format!(
        r#"{{"preparation_id":"{prep_id}","consent_token":"{consent}","grants":[],"state_policy":"reset","target_config_digest":"{digest}"}}"#
    );
    let make_raw_req = |raw: String| {
        axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/plugins/org.example.canon/upgrade")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .header("if-match", format!("\"{rev1}\""))
            .header("idempotency-key", "canon-upg-exec")
            .body(axum::body::Body::from(raw))
            .unwrap()
    };

    let (st1, _, b1) = env.send(make_raw_req(raw1)).await;
    let op1 = assert_ok(st1, &b1, StatusCode::ACCEPTED);
    let (st2, _, b2) = env.send(make_raw_req(raw2)).await;
    let op2 = assert_ok(st2, &b2, StatusCode::ACCEPTED);
    assert_eq!(op1["operation_id"], op2["operation_id"]);

    let (st3, _, b3) = env.send(make_raw_req(raw_diff)).await;
    assert_err(
        st3,
        &b3,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );
}

#[tokio::test]
async fn prepare_upgrade_resolves_semver_rollback_without_package_hash() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.rbver";
    let pkg1 = build_task_package(pid, "1.0.0", 1, false);
    let d1 = env.install_pkg(&pkg1, None, json!([])).await;
    let rev1 = d1["revision"].as_i64().unwrap();
    let srev = plugin_http_support::pkg::schema_rev(false, None);

    let pkg2 = build_task_package(pid, "2.0.0", 1, false);
    let up2 = env.upload_pkg(&pkg2, None, "rbver-up").await;
    let (st_p2, _, b_p2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev1}\"")),
                ("idempotency-key", "rbv-p2"),
            ],
            Some(json!({
                "version": "2.0.0",
                "upload_id": up2["upload_id"],
                "target_config": { "schema_revision": srev, "values": { "mode": "v2" } },
                "state_policy": "preserve"
            })),
        ))
        .await;
    let prep2 = assert_ok(st_p2, &b_p2, StatusCode::ACCEPTED)["result"].clone();
    let reauth = env.reauth("plugin:upgrade").await;
    let (st_u2, _, b_u2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrade"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev1}\"")),
                ("idempotency-key", "rbv-u2"),
            ],
            Some(json!({
                "preparation_id": prep2["preparation_id"],
                "consent_token": prep2["consent_token"],
                "grants": [],
                "state_policy": "preserve",
                "target_config_digest": prep2["config_diff"]["target_config_digest"],
                "reauth_token": reauth
            })),
        ))
        .await;
    assert_eq!(
        assert_ok(st_u2, &b_u2, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );

    let (_, _, b_g) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev2 = b_g["data"]["revision"].as_i64().unwrap();
    let (st_rb, _, b_rb) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev2}\"")),
                ("idempotency-key", "rbv-rb"),
            ],
            Some(json!({
                "version": "1.0.0",
                "state_policy": "preserve",
                "target_config": { "schema_revision": srev, "values": { "mode": "ok" } }
            })),
        ))
        .await;
    let rb_prep = assert_ok(st_rb, &b_rb, StatusCode::ACCEPTED)["result"].clone();
    assert_eq!(rb_prep["version"], "1.0.0");

    let (st_bad, _, b_bad) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev2}\"")),
                ("idempotency-key", "rbv-bad"),
            ],
            Some(json!({
                "version": "1.0.0",
                "upload_id": up2["upload_id"],
                "package_hash": d1["package_hash"],
                "state_policy": "preserve",
                "target_config": {}
            })),
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
}
