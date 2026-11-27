use std::time::{Duration, Instant};

use axum::http::StatusCode;
use domain::DownloaderId;
use serde_json::{Value, json};
use store::StoreError;
use store::plugins::{
    AcceptStagedPluginOperation, PluginCommandCause, PluginError, PluginSubject,
    PluginVersionMetadata, StagedPluginIntent, StagedPluginOpKind,
};
use uuid::Uuid;

use crate::support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_multipart,
};
use crate::{canon_req, post_prep, post_write};

fn task_pkg(id: &str, version: &str, perms: &[&str]) -> Vec<u8> {
    pkg::build_task_package_custom(id, version, 1, false, None, false, perms)
}

fn dl_grant(cap: &str, id: impl Into<String>) -> Value {
    json!([{
        "capability": cap,
        "scope": { "Object": { "kind": "downloader", "id": id.into() } },
        "sensitive": false
    }])
}

#[tokio::test]
async fn slice_d_r4_item1a_register_staged_version_enforces_activation_policy() {
    let env = HttpEnv::new().await;
    let store = env.state.store();

    let pid = "org.example.r4-staged-pol";
    let hash = "r4-staged-hash-1";
    let mut claim = store
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r4-sp-1".into(),
                request: canon_req("POST", "/inst", 1),
                op_kind: StagedPluginOpKind::Install,
                expected_revision: 0,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(hash.into()),
                    version: Some("1.0.0".into()),
                    artifact_path: None,
                    state_policy: None,
                    target_config_digest: None,
                    grants: Vec::new(),
                },
                cause: PluginCommandCause::root("r4-sp:1"),
            })?;
            app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                acc.operation.revision,
                0,
                1_700_000_000,
            )
        })
        .unwrap();

    let unsigned_ver = PluginVersionMetadata {
        plugin_id: pid.into(),
        package_hash: hash.into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "plugins/packages/unused".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    };
    let err = store
        .register_staged_plugin_version(&mut claim, &unsigned_ver, &[], 1_700_000_001)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Plugin(PluginError::Denied)),
        "expected Denied when developer_mode is off, got {err:?}"
    );
}

#[tokio::test]
async fn slice_d_r4_item1b_stage_upload_rechecks_dev_mode_and_trust_key_in_write_tx() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let zip = task_pkg("org.example.r4-up-race", "1.0.0", &[]);
    let upload_id = Uuid::new_v4();
    let reauth = env.reauth("plugin:upload").await;
    let up_dir = store
        .data_dir()
        .join("plugins/uploads")
        .join(upload_id.to_string());

    let store_bg = store.clone();
    let up_dir_bg = up_dir.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let bg = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let _ = entered_tx.send(());
                let deadline = Instant::now() + Duration::from_secs(5);
                while !up_dir_bg.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
                let cur = app.plugin_developer_mode()?;
                app.set_plugin_developer_mode(false, cur.revision, &admin_id().to_string())?;
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();

    let uid_s = upload_id.to_string();
    let parts = [
        ("upload_id", None, uid_s.as_bytes()),
        ("file", Some("plugin.cmplugin"), zip.as_slice()),
        ("reauth_token", None, reauth.as_bytes()),
    ];
    let req = req_multipart(
        "/api/v1/plugin-uploads",
        Some(TOKEN),
        &[("if-match", "\"0\""), ("idempotency-key", "r4-up-dev-race")],
        "----R4Boundary",
        &parts,
    );
    let (st, _, body) = env.send(req).await;
    bg.join().unwrap();

    assert_err(
        st,
        &body,
        StatusCode::FORBIDDEN,
        "plugin.signature_required",
    );
    assert!(
        !up_dir.exists(),
        "upload dir must be cleaned up on trust failure"
    );
    assert!(
        env.state
            .plugins()
            .peek_reauth_token(
                &env.state,
                admin_id(),
                api::plugins::ReauthPurpose::PluginUpload,
                &reauth,
            )
            .await
            .is_ok(),
        "reauth token must not be consumed when trust recheck fails"
    );

    assert_signed_upload_revoked_key_race(&env).await;
}

async fn assert_signed_upload_revoked_key_race(env: &HttpEnv) {
    let store = env.state.store();
    let key = pkg::TestSigningKey::generate(9401);
    let pinned = env.pin_trust_key(&key, "r4-sign-race").await;
    let kid = pinned["key_id"].as_str().unwrap().to_string();
    let krev = pinned["revision"].as_i64().unwrap();

    let zip = task_pkg("org.example.r4-up-sig-race", "1.0.0", &[]);
    let sig = key.sign_package(&zip);
    let upload_id = Uuid::new_v4();
    let reauth = env.reauth("plugin:upload").await;
    let up_dir = store
        .data_dir()
        .join("plugins/uploads")
        .join(upload_id.to_string());

    let store_bg = store.clone();
    let up_dir_bg = up_dir.clone();
    let kid_bg = kid.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let bg = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let _ = entered_tx.send(());
                let deadline = Instant::now() + Duration::from_secs(5);
                while !up_dir_bg.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
                app.revoke_plugin_trust_key(
                    &kid_bg,
                    Some(krev),
                    "compromised",
                    &admin_id().to_string(),
                )?;
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();

    let uid_s = upload_id.to_string();
    let parts = [
        ("upload_id", None, uid_s.as_bytes()),
        ("file", Some("plugin.cmplugin"), zip.as_slice()),
        ("signature", Some("plugin.minisig"), sig.as_bytes()),
        ("reauth_token", None, reauth.as_bytes()),
    ];
    let req = req_multipart(
        "/api/v1/plugin-uploads",
        Some(TOKEN),
        &[("if-match", "\"0\""), ("idempotency-key", "r4-up-sig-race")],
        "----R4SigBoundary",
        &parts,
    );
    let (st, _, body) = env.send(req).await;
    bg.join().unwrap();

    assert_err(st, &body, StatusCode::FORBIDDEN, "plugin.key_revoked");
    assert!(!up_dir.exists());
    assert!(
        env.state
            .plugins()
            .peek_reauth_token(
                &env.state,
                admin_id(),
                api::plugins::ReauthPurpose::PluginUpload,
                &reauth,
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn slice_d_r4_item1c_downloader_grant_scope_validated_and_switch_rolls_back_version() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r4-dl-sw";
    let z1 = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["read:boost"]);
    let rev1 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let prep_path = format!("/api/v1/plugins/{pid}/permissions/prepare");
    for bad in [
        "not-a-uuid".to_string(),
        format!("downloader:{}", DownloaderId::new()),
    ] {
        let (st, b) = post_prep(
            &env,
            &prep_path,
            json!({ "requested_grants": dl_grant("read:boost", bad) }),
        )
        .await;
        assert_err(st, &b, StatusCode::BAD_REQUEST, "plugin.scope_invalid");
    }

    let z2 = pkg::build_task_package_custom(pid, "1.1.0", 2, false, None, false, &["read:boost"]);
    let up2 = env.upload_pkg(&z2, None, "r4-up-z2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    let hash2 = up2["hash"].as_str().unwrap().to_string();

    // Prepare with state_policy = "reset" (since state_schema 1 -> 2), then tamper DB current
    // version state_schema or prepare with preserve after setting current state_schema = "2"
    // and reverting it before execute_upgrade so apply_plugin_package_switch hits StateSchemaMismatch!
    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE plugin_versions SET state_schema='2' WHERE plugin_id=?1",
                [pid],
            )?;
            Ok(())
        })
        .unwrap();
    let srev = pkg::schema_rev(false, None);
    let (st_pu, b_pu) = post_write(
        &env,
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev1,
        "r4-pu-atomic",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "state_policy": "preserve",
            "target_config": { "schema_revision": srev, "values": { "mode": "ok" } }
        }),
    )
    .await;
    let prep_u = assert_ok(st_pu, &b_pu, StatusCode::ACCEPTED)["result"].clone();

    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE plugin_versions SET state_schema='1' WHERE plugin_id=?1",
                [pid],
            )?;
            Ok(())
        })
        .unwrap();

    let (st_ex, b_ex) = post_write(
        &env,
        format!("/api/v1/plugins/{pid}/upgrade"),
        rev1,
        "r4-ex-atomic",
        json!({
            "preparation_id": prep_u["preparation_id"],
            "consent_token": prep_u["consent_token"],
            "grants": [],
            "state_policy": "preserve",
            "target_config_digest": prep_u["target_config_digest"],
        }),
    )
    .await;
    assert_err(
        st_ex,
        &b_ex,
        StatusCode::CONFLICT,
        "plugin.state_schema_mismatch",
    );
    assert!(
        store.get_plugin_version(pid, &hash2).unwrap().is_none(),
        "failed switch must roll back newly registered plugin_versions row"
    );
    assert!(
        !store
            .data_dir()
            .join("plugins/packages")
            .join(&hash2)
            .exists(),
        "unreferenced package directory must be GC'd after rolled-back switch"
    );
}
