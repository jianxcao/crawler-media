use std::time::Duration;

use api::plugins::entrypoints::recover_plugin_startup_for_test;
use axum::http::StatusCode;
use domain::SiteId;
use serde_json::json;
use store::plugins::{
    AcceptStagedPluginOperation, PluginCommandCause, PluginOperationState, PluginSubject,
    StagedPluginIntent, StagedPluginOpKind,
};

use crate::support::plugin_http_support::{HttpEnv, admin_id, assert_err, assert_ok, pkg};
use crate::{get_json, http_canon_req, post_plain, rotate_site_credentials, send_write};

#[tokio::test]
async fn slice_d_r5_item3a_credential_bound_config_update_get_validate_and_enable_isolate_scopes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();
    let site_a = env.insert_site("R5-CfgSiteA");
    let site_b = env.insert_site("R5-CfgSiteB");

    let pid = "org.example.r5-cb-cfg";
    let zip = pkg::build_task_package_custom(pid, "1.0.0", 1, true, None, true, &["kv"]);
    let srev = pkg::schema_rev(true, None);
    let rev0 = env.install_pkg(&zip, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let cfg_path = format!("/api/v1/plugins/{pid}/config");
    let (st_a, b_a) = send_write(
        &env,
        "PUT",
        &cfg_path,
        rev0,
        "r5-cfg-a1",
        json!({
            "schema_revision": srev,
            "values": { "mode": "site-a" },
            "secrets": { "api_secret": { "action": "replace", "value": "secret-a-v1" } },
            "site_id": site_a,
        }),
    )
    .await;
    let rev1 = assert_ok(st_a, &b_a, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let (st_b_keep, b_b_keep) = send_write(
        &env,
        "PUT",
        &cfg_path,
        rev1,
        "r5-cfg-b-keep",
        json!({
            "schema_revision": srev,
            "values": { "mode": "site-b" },
            "secrets": { "api_secret": { "action": "keep" } },
            "site_id": site_b,
        }),
    )
    .await;
    assert_err(
        st_b_keep,
        &b_b_keep,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );

    rotate_site_credentials(&store, site_a);
    assert_dead_epoch_rejected_across_config_and_enable(&env, pid, &srev, site_a, rev1).await;
}

async fn assert_dead_epoch_rejected_across_config_and_enable(
    env: &HttpEnv,
    pid: &str,
    srev: &str,
    site_a: SiteId,
    rev: i64,
) {
    let cfg_path = format!("/api/v1/plugins/{pid}/config");
    let (st_get, b_get) = get_json(env, &cfg_path).await;
    let got = assert_ok(st_get, &b_get, StatusCode::OK);
    assert_eq!(got["secrets"]["api_secret"]["configured"], false);

    let (st_val, b_val) = post_plain(
        env,
        format!("/api/v1/plugins/{pid}/config/validate"),
        json!({ "schema_revision": srev, "values": { "mode": "check" }, "secrets": {} }),
    )
    .await;
    assert_err(
        st_val,
        &b_val,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );

    for (idem, body) in [
        (
            "r5-dead-keep",
            json!({
                "schema_revision": srev,
                "values": { "mode": "dead-a" },
                "secrets": { "api_secret": { "action": "keep" } },
                "site_id": site_a,
            }),
        ),
        (
            "r5-dead-pub",
            json!({
                "schema_revision": srev,
                "values": { "mode": "dead-pub" },
                "secrets": {},
            }),
        ),
    ] {
        let (st_u, b_u) = send_write(env, "PUT", &cfg_path, rev, idem, body).await;
        assert_err(st_u, &b_u, StatusCode::BAD_REQUEST, "plugin.config_invalid");
    }

    let (st_en, b_en) = send_write(
        env,
        "POST",
        format!("/api/v1/plugins/{pid}/enable"),
        rev,
        "r5-dead-en",
        json!({}),
    )
    .await;
    let op = assert_ok(st_en, &b_en, StatusCode::ACCEPTED);
    assert_eq!(op["state"], "failed");
    assert_eq!(op["phase"], "config-invalid");
}

#[tokio::test]
async fn slice_d_r5_item3b_upgrade_prepare_switch_and_rollforward_enforce_live_secret_scopes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();
    let site_a = env.insert_site("R5-UpgSiteA");

    let pid = "org.example.r5-upg-sec";
    let z1 = pkg::build_task_package_custom(pid, "1.0.0", 1, true, None, false, &["kv"]);
    let srev = pkg::schema_rev(true, None);
    let rev0 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let (st_c, b_c) = send_write(
        &env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        rev0,
        "r5-u-gcfg",
        json!({
            "schema_revision": srev,
            "values": { "mode": "v1" },
            "secrets": { "api_secret": { "action": "replace", "value": "global-secret" } },
        }),
    )
    .await;
    let rev1 = assert_ok(st_c, &b_c, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let z2 = pkg::build_task_package_custom(pid, "1.1.0", 1, true, None, true, &["kv"]);
    let up2 = env.upload_pkg(&z2, None, "r5-up-z2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    assert_global_to_site_upgrade_prep_rejected(&env, pid, uid2, &srev, site_a, rev1).await;

    let prep_path = format!("/api/v1/plugins/{pid}/upgrades/prepare");
    let (st_ok, b_ok) = send_write(
        &env,
        "POST",
        &prep_path,
        rev1,
        "r5-pu-ok",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "site_id": site_a,
            "state_policy": "reset",
            "target_config": {
                "schema_revision": srev,
                "values": { "mode": "v2" },
                "secrets": { "api_secret": { "action": "replace", "value": "site-a-v2" } },
            }
        }),
    )
    .await;
    let prep = assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED)["result"].clone();
    rotate_site_credentials(&store, site_a);
    let reauth = env.reauth("plugin:upgrade").await;
    let (st_ex, b_ex) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrade"),
        rev1,
        "r5-ex-stale",
        json!({
            "preparation_id": prep["preparation_id"],
            "consent_token": prep["consent_token"],
            "grants": [],
            "state_policy": "reset",
            "target_config_digest": prep["target_config_digest"],
            "reauth_token": reauth,
        }),
    )
    .await;
    assert_err(st_ex, &b_ex, StatusCode::CONFLICT, "plugin.consent_stale");

    assert_rollforward_rejects_rotated_secret_scope(&env).await;
}

async fn assert_global_to_site_upgrade_prep_rejected(
    env: &HttpEnv,
    pid: &str,
    uid2: &str,
    srev: &str,
    site_a: SiteId,
    rev1: i64,
) {
    let prep_path = format!("/api/v1/plugins/{pid}/upgrades/prepare");
    let (st_no_site, b_no_site) = send_write(
        env,
        "POST",
        &prep_path,
        rev1,
        "r5-pu-nosite",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "state_policy": "reset",
            "target_config": {
                "schema_revision": srev,
                "values": { "mode": "v2" },
                "secrets": { "api_secret": { "action": "replace", "value": "illegal-global" } },
            }
        }),
    )
    .await;
    assert_err(
        st_no_site,
        &b_no_site,
        StatusCode::BAD_REQUEST,
        "plugin.scope_invalid",
    );

    let (st_keep_g, b_keep_g) = send_write(
        env,
        "POST",
        &prep_path,
        rev1,
        "r5-pu-keepg",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "site_id": site_a,
            "state_policy": "reset",
            "target_config": {
                "schema_revision": srev,
                "values": { "mode": "v2" },
                "secrets": { "api_secret": { "action": "keep" } },
            }
        }),
    )
    .await;
    assert_err(
        st_keep_g,
        &b_keep_g,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );
}

async fn assert_rollforward_rejects_rotated_secret_scope(env: &HttpEnv) {
    let store = env.state.store();
    let site_rf = env.insert_site("R5-RollforwardSite");
    let pid = "org.example.r5-rf-sec";
    let z1 = pkg::build_task_package_custom(pid, "1.0.0", 1, true, None, true, &["kv"]);
    let z2 = pkg::build_task_package_custom(pid, "1.1.0", 1, true, None, true, &["kv"]);
    let srev = pkg::schema_rev(true, None);
    let rev0 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let (st_c, b_c) = send_write(
        env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        rev0,
        "r5-rf-cfg",
        json!({
            "schema_revision": srev,
            "values": { "mode": "rf" },
            "secrets": { "api_secret": { "action": "replace", "value": "rf-secret" } },
            "site_id": site_rf,
        }),
    )
    .await;
    let rev1 = assert_ok(st_c, &b_c, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let up2 = env.upload_pkg(&z2, None, "r5-rf-up2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    let hash2 = up2["hash"].as_str().unwrap().to_string();
    let (st_p, b_p) = send_write(
        env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev1,
        "r5-rf-prep",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "state_policy": "preserve",
            "target_config": { "schema_revision": srev, "values": { "mode": "rf" }, "secrets": {} }
        }),
    )
    .await;
    let digest = assert_ok(st_p, &b_p, StatusCode::ACCEPTED)["result"]["target_config_digest"]
        .as_str()
        .unwrap()
        .to_string();

    let rel_art = format!("plugins/uploads/{uid2}/package.cmplugin");
    let op_id = stage_running_preserve_upgrade(&store, pid, rev1, &hash2, &rel_art, &digest);
    rotate_site_credentials(&store, site_rf);
    recover_plugin_startup_for_test(&env.state).unwrap();

    let after = store.get_plugin(pid).unwrap().unwrap();
    assert_ne!(after.package_hash, hash2);
    let admin_op = store.get_plugin_operation_admin(op_id).unwrap().unwrap();
    assert_eq!(admin_op.operation.state, PluginOperationState::Failed);
}

fn stage_running_preserve_upgrade(
    store: &store::Store,
    pid: &str,
    rev: i64,
    hash2: &str,
    rel_art: &str,
    digest: &str,
) -> uuid::Uuid {
    store
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r5-rf-op".into(),
                request: http_canon_req("POST", &format!("/plugins/{pid}/upgrade"), &json!({})),
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: rev,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(hash2.into()),
                    version: Some("1.1.0".into()),
                    artifact_path: Some(rel_art.into()),
                    state_policy: Some("preserve".into()),
                    target_config_digest: Some(digest.into()),
                    grants: Vec::new(),
                },
                cause: PluginCommandCause::root("r5-rf"),
            })?;
            let _ = app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                acc.operation.revision,
                0,
                1_700_000_000,
            )?;
            Ok(acc.operation.operation_id)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_d_r5_item3c_prepare_upgrade_inflight_wal_commit_replays_via_after_write() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r5-wal-up";
    let z1 = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let z2 = pkg::build_task_package_custom(pid, "1.1.0", 1, false, None, false, &["kv"]);
    let srev = pkg::schema_rev(false, None);
    let rev1 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let up2 = env.upload_pkg(&z2, None, "r5-wal-z2").await;
    let uid2 = up2["upload_id"].as_str().unwrap().to_string();
    let hash2 = up2["hash"].as_str().unwrap().to_string();

    let updated = store
        .save_plugin_config(pid, rev1, &srev, &json!({ "mode": "ok" }))
        .unwrap();
    let rev2 = updated.revision;

    let idem = "r5-wal-up-idem";
    let body = json!({
        "version": "1.1.0",
        "upload_id": uid2,
        "state_policy": "preserve",
        "target_config": { "schema_revision": srev, "values": { "mode": "ok" }, "secrets": {} }
    });
    let creq = http_canon_req("POST", &format!("/plugins/{pid}/upgrades/prepare"), &body);

    let store_bg = store.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let bg = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                    subject: PluginSubject::User(admin_id()),
                    idempotency_key: idem.into(),
                    request: creq,
                    op_kind: StagedPluginOpKind::UpgradePrepare,
                    expected_revision: rev2,
                    intent: StagedPluginIntent {
                        target_plugin_id: pid.into(),
                        package_hash: Some(hash2),
                        version: Some("1.1.0".into()),
                        artifact_path: None,
                        state_policy: Some("preserve".into()),
                        target_config_digest: Some("sha256:00".into()),
                        grants: Vec::new(),
                    },
                    cause: PluginCommandCause::root(format!("upgrade-prepare:{pid}")),
                })?;
                let op_id = acc.operation.operation_id;
                let claim = app.claim_staged_plugin_operation(
                    op_id,
                    acc.operation.revision,
                    0,
                    1_700_000_000,
                )?;
                app.complete_staged_plugin_operation(
                    &claim,
                    PluginOperationState::Succeeded,
                    "prepared",
                    Some(&json!({ "plugin_id": pid })),
                    None,
                    1_700_000_001,
                )?;
                let _ = entered_tx.send(op_id);
                std::thread::sleep(Duration::from_millis(60));
                Ok(())
            })
            .unwrap();
    });
    let expected_op = entered_rx.recv().unwrap();

    let (st_pu, b_pu) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev1,
        idem,
        body,
    )
    .await;
    bg.join().unwrap();
    let replayed = assert_ok(st_pu, &b_pu, StatusCode::ACCEPTED);
    assert_eq!(replayed["operation_id"], expected_op.to_string());
}
