#[path = "plugin_p1c_support/mod.rs"]
mod plugin_p1c_support;

use std::collections::BTreeMap;

use api::plugins::*;
use plugin_p1c_support::pkg::{
    TestSigningKey, build_task_package, build_task_package_custom, schema_rev,
};
use plugin_p1c_support::{
    admin_id, build_state, create_trust_key_req, disable_plugin_ok, enable_dev_mode,
    enable_plugin_ok, exec_rollback_req, exec_state_reset_req, exec_upgrade_req, issue_reauth,
    pin_trust_key, prepare_state_reset_ok, prepare_upgrade_ok, reconcile_schedule_cmd, req_ctx,
    stage_upload_req, upload_and_install, upload_pkg,
};
use plugin_package::SecretWriteAction;
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycle, PluginOperationState, PluginStateKind,
};

fn put_global_kv(state: &api::ApiState, plugin_id: &str, key: &str, val: &[u8]) -> i64 {
    let p = state.store().get_plugin(plugin_id).unwrap().unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    state
        .store()
        .plugin_state_put(&auth, PluginStateKind::Kv, key, val, 0)
        .unwrap();
    p.revision
}

#[tokio::test]
async fn upgrade_preserve_rejects_state_schema_mismatch_and_reset_clears_kv() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let key = TestSigningKey::generate(99);
    pin_trust_key(&state, &key).await;

    let pkg1 = build_task_package("org.example.upg", "1.0.0", 1, false);
    let sig1 = key.sign_package(&pkg1);
    let grants = vec![PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let p1 = upload_and_install(&state, pkg1, Some(sig1), grants.clone()).await;
    enable_plugin_ok(&state, "org.example.upg", p1.revision, "en-upg").await;
    let p1_rev = put_global_kv(&state, "org.example.upg", "persist", b"v1");

    let pkg2 = build_task_package("org.example.upg", "1.1.0", 2, false);
    let sig2 = key.sign_package(&pkg2);
    let u2 = upload_pkg(&state, pkg2, Some(sig2)).await.unwrap();
    let schema = state
        .plugins()
        .get_config_schema(&state, "org.example.upg")
        .await
        .unwrap();
    let (pk_bad, pr_bad) = req_ctx(
        "POST",
        "/plugins/org.example.upg/upgrades/prepare",
        "up-bad",
        12,
    );
    let err_pres = state
        .plugins()
        .prepare_upgrade(
            &state,
            "org.example.upg",
            PrepareUpgradeInput {
                actor_user_id: admin_id(),
                version: "1.1.0".into(),
                upload_id: Some(u2.upload_id),
                package_hash: None,
                repo_id: None,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: schema.schema_revision,
                    values: serde_json::json!({"mode": "v2"}),
                    secrets: BTreeMap::new(),
                },
                site_id: None,
                state_policy: "preserve".into(),
                reset_scopes: None,
                expected_revision: p1_rev,
                idempotency_key: pk_bad,
                request: pr_bad,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err_pres.status_and_code(),
        (409, "plugin.state_schema_mismatch")
    );

    let prep = prepare_upgrade_ok(
        &state,
        "org.example.upg",
        "1.1.0",
        (Some(u2.upload_id), None),
        ("reset", "v2", p1_rev, "up-ok"),
    )
    .await;
    assert_eq!(
        prep.state_diff.reset_preview.as_ref().unwrap().kv_entries,
        1
    );

    let no_reauth = exec_upgrade_req(
        &state,
        "org.example.upg",
        prep.clone(),
        grants.clone(),
        ("reset", None, p1_rev, "up-no"),
    )
    .await
    .unwrap_err();
    assert_eq!(no_reauth.status_and_code().0, 403);

    let reauth = issue_reauth(&state, ReauthPurpose::PluginUpgrade).await;
    let upg_op = exec_upgrade_req(
        &state,
        "org.example.upg",
        prep,
        grants,
        ("reset", Some(reauth), p1_rev, "up-exec"),
    )
    .await
    .unwrap();
    assert_eq!(upg_op.state, PluginOperationState::Succeeded);
    let p2 = state
        .plugins()
        .get_plugin(&state, "org.example.upg")
        .await
        .unwrap();
    assert_eq!(p2.version, "1.1.0");
    assert_eq!(p2.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(
        state
            .store()
            .preview_plugin_state_reset("org.example.upg", None)
            .unwrap()
            .kv_entries,
        0
    );
}

#[tokio::test]
async fn rollback_restores_historical_package_and_rejects_upload_source() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let key = TestSigningKey::generate(101);
    pin_trust_key(&state, &key).await;

    let pkg1 = build_task_package("org.example.rb", "1.0.0", 1, false);
    let sig1 = key.sign_package(&pkg1);
    let grants = vec![PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let p1 = upload_and_install(&state, pkg1, Some(sig1), grants.clone()).await;

    let pkg2 = build_task_package("org.example.rb", "1.1.0", 1, false);
    let sig2 = key.sign_package(&pkg2);
    let u2 = upload_pkg(&state, pkg2, Some(sig2)).await.unwrap();
    let up_prep = prepare_upgrade_ok(
        &state,
        "org.example.rb",
        "1.1.0",
        (Some(u2.upload_id), None),
        ("preserve", "v2", p1.revision, "rb-up-p"),
    )
    .await;
    exec_upgrade_req(
        &state,
        "org.example.rb",
        up_prep,
        grants.clone(),
        ("preserve", None, p1.revision, "rb-up-e"),
    )
    .await
    .unwrap();
    let p2 = state
        .plugins()
        .get_plugin(&state, "org.example.rb")
        .await
        .unwrap();

    let pkg3 = build_task_package("org.example.rb", "1.2.0", 1, false);
    let sig3 = key.sign_package(&pkg3);
    let u3 = upload_pkg(&state, pkg3, Some(sig3)).await.unwrap();
    let upload_prep = prepare_upgrade_ok(
        &state,
        "org.example.rb",
        "1.2.0",
        (Some(u3.upload_id), None),
        ("reset", "v3", p2.revision, "rb-bad-p"),
    )
    .await;
    let rb_reauth = issue_reauth(&state, ReauthPurpose::PluginRollback).await;
    let bad_rb = exec_rollback_req(
        &state,
        "org.example.rb",
        &upload_prep.package_hash.clone(),
        upload_prep,
        grants.clone(),
        ("reset", Some(rb_reauth.clone()), p2.revision, "rb-bad-e"),
    )
    .await
    .unwrap_err();
    assert_eq!(bad_rb.status_and_code(), (409, "plugin.consent_stale"));

    let rb_prep = prepare_upgrade_ok(
        &state,
        "org.example.rb",
        "1.0.0",
        (None, Some(p1.package_hash.clone())),
        ("reset", "v1", p2.revision, "rb-ok-p"),
    )
    .await;
    let rb_op = exec_rollback_req(
        &state,
        "org.example.rb",
        &p1.package_hash,
        rb_prep,
        grants,
        ("reset", Some(rb_reauth), p2.revision, "rb-ok-e"),
    )
    .await
    .unwrap();
    assert_eq!(rb_op.state, PluginOperationState::Succeeded);
    let p_back = state
        .plugins()
        .get_plugin(&state, "org.example.rb")
        .await
        .unwrap();
    assert_eq!(p_back.package_hash, p1.package_hash);
}

#[tokio::test]
async fn failed_upgrade_at_lifecycle_validate_settles_failed_and_clears_active_op() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg1 = build_task_package("org.example.upfail", "1.0.0", 1, false);
    let p1 = upload_and_install(&state, pkg1, None, vec![]).await;
    enable_plugin_ok(&state, "org.example.upfail", p1.revision, "en-upfail-1").await;
    let p1_en = state
        .plugins()
        .get_plugin(&state, "org.example.upfail")
        .await
        .unwrap();

    let pkg2 = build_task_package_custom(
        "org.example.upfail",
        "1.1.0",
        1,
        false,
        Some("reject"),
        false,
        &["kv"],
    );
    let u2 = upload_pkg(&state, pkg2, None).await.unwrap();
    let (pk, pr) = req_ctx(
        "POST",
        "/plugins/org.example.upfail/upgrades/prepare",
        "upfail-p",
        13,
    );
    let prep_op = state
        .plugins()
        .prepare_upgrade(
            &state,
            "org.example.upfail",
            PrepareUpgradeInput {
                actor_user_id: admin_id(),
                version: "1.1.0".into(),
                upload_id: Some(u2.upload_id),
                package_hash: None,
                repo_id: None,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: schema_rev(false, Some("reject")),
                    values: serde_json::json!({"mode": "ok"}),
                    secrets: BTreeMap::new(),
                },
                site_id: None,
                state_policy: "preserve".into(),
                reset_scopes: None,
                expected_revision: p1_en.revision,
                idempotency_key: pk,
                request: pr,
            },
        )
        .await
        .unwrap();
    let prep: UpgradePreparationView = serde_json::from_value(prep_op.result.unwrap()).unwrap();
    let err = exec_upgrade_req(
        &state,
        "org.example.upfail",
        prep,
        vec![],
        ("preserve", None, p1_en.revision, "upfail-e"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_and_code(), (400, "plugin.invalid_component"));

    let ops = state
        .store()
        .list_plugin_operations_admin(Some("org.example.upfail"), None, None, Some(20))
        .unwrap();
    assert_eq!(
        ops.items
            .iter()
            .find(|o| o.kind == "upgrade")
            .unwrap()
            .state,
        PluginOperationState::Failed
    );
    let after = state
        .plugins()
        .get_plugin(&state, "org.example.upfail")
        .await
        .unwrap();
    assert_eq!(after.active_operation_id, None);
    assert_eq!(after.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(after.package_hash, p1.package_hash);
    enable_plugin_ok(&state, "org.example.upfail", after.revision, "en-upfail-2").await;
}

#[tokio::test]
async fn prepare_upgrade_preserve_defaults_credential_bound_site_and_grant_diff_added() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let site = plugin_p1c_support::site();
    state.store().insert_site(&site).unwrap();
    let pkg1 =
        build_task_package_custom("org.example.cbupg", "1.0.0", 1, false, None, true, &["kv"]);
    let p1 = upload_and_install(&state, pkg1, None, vec![]).await;
    let pkg2 = build_task_package_custom(
        "org.example.cbupg",
        "1.1.0",
        1,
        true,
        None,
        true,
        &["kv", "blob"],
    );
    let u2 = upload_pkg(&state, pkg2, None).await.unwrap();
    let target_rev = schema_rev(true, None);
    let mut secrets = BTreeMap::new();
    secrets.insert(
        "api_secret".into(),
        SecretWriteAction::Replace("secret-site-val".into()),
    );

    let (pk_bad, pr_bad) = req_ctx(
        "POST",
        "/plugins/org.example.cbupg/upgrades/prepare",
        "cb-bad",
        5,
    );
    let bad_site = state
        .plugins()
        .prepare_upgrade(
            &state,
            "org.example.cbupg",
            PrepareUpgradeInput {
                actor_user_id: admin_id(),
                version: "1.1.0".into(),
                upload_id: Some(u2.upload_id),
                package_hash: None,
                repo_id: None,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: target_rev.clone(),
                    values: serde_json::json!({"mode": "ok"}),
                    secrets: secrets.clone(),
                },
                site_id: Some(domain::SiteId::new()),
                state_policy: "reset".into(),
                reset_scopes: None,
                expected_revision: p1.revision,
                idempotency_key: pk_bad,
                request: pr_bad,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad_site.status_and_code(), (400, "plugin.scope_invalid"));

    let (pk_ok, pr_ok) = req_ctx(
        "POST",
        "/plugins/org.example.cbupg/upgrades/prepare",
        "cb-ok",
        6,
    );
    let prep_op = state
        .plugins()
        .prepare_upgrade(
            &state,
            "org.example.cbupg",
            PrepareUpgradeInput {
                actor_user_id: admin_id(),
                version: "1.1.0".into(),
                upload_id: Some(u2.upload_id),
                package_hash: None,
                repo_id: None,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: target_rev,
                    values: serde_json::json!({"mode": "ok"}),
                    secrets,
                },
                site_id: Some(site.id),
                state_policy: "reset".into(),
                reset_scopes: None,
                expected_revision: p1.revision,
                idempotency_key: pk_ok,
                request: pr_ok,
            },
        )
        .await
        .unwrap();
    let prep: UpgradePreparationView = serde_json::from_value(prep_op.result.unwrap()).unwrap();
    assert_eq!(prep.grant_diff.added.len(), 2);

    let pkg1b = build_task_package("org.example.presdef", "1.0.0", 1, false);
    let pb = upload_and_install(&state, pkg1b, None, vec![]).await;
    let pkg2b = build_task_package("org.example.presdef", "1.1.0", 1, false);
    let u2b = upload_pkg(&state, pkg2b, None).await.unwrap();
    let pres_prep = prepare_upgrade_ok(
        &state,
        "org.example.presdef",
        "1.1.0",
        (Some(u2b.upload_id), None),
        ("preserve", "ok", pb.revision, "pres-def-1"),
    )
    .await;
    assert!(pres_prep.state_diff.preserve_allowed);
    assert!(pres_prep.config_diff.existing_valid);
}

#[tokio::test]
async fn stale_consent_preserves_reauth_on_trust_key_and_upload() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let key = TestSigningKey::generate(303);
    let prep_tk = state
        .plugins()
        .prepare_trust_key(
            &state,
            admin_id(),
            PrepareTrustKeyInput {
                public_key: key.public_key_str.clone(),
                label: "Peek Key".into(),
            },
        )
        .await
        .unwrap();
    let reauth_tk = issue_reauth(&state, ReauthPurpose::TrustKeyCreate).await;
    let err = create_trust_key_req(
        &state,
        &prep_tk,
        "wrong-consent".into(),
        reauth_tk.clone(),
        "peek-tk-bad",
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.consent_stale"));
    create_trust_key_req(
        &state,
        &prep_tk,
        prep_tk.consent_token.clone(),
        reauth_tk,
        "peek-tk-ok",
    )
    .await
    .unwrap();

    let up_reauth = issue_reauth(&state, ReauthPurpose::PluginUpload).await;
    let bad_up = stage_upload_req(
        &state,
        b"not-a-zip".to_vec(),
        up_reauth.clone(),
        "peek-up-bad",
    )
    .await
    .unwrap_err();
    assert_eq!(bad_up.status_and_code().0, 400);
    let pkg = build_task_package("org.example.peek", "1.0.0", 1, false);
    stage_upload_req(&state, pkg, up_reauth, "peek-up-ok")
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_consent_preserves_reauth_on_state_reset_and_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg1 = build_task_package("org.example.peek2", "1.0.0", 1, false);
    let detail = upload_and_install(&state, pkg1, None, vec![]).await;
    let sr_prep = prepare_state_reset_ok(&state, &detail.id).await;
    let sr_reauth = issue_reauth(&state, ReauthPurpose::PluginStateReset).await;
    let sr_err = exec_state_reset_req(
        &state,
        &detail.id,
        "bad-consent".into(),
        sr_reauth.clone(),
        detail.revision,
        "peek-sr-bad",
    )
    .await
    .unwrap_err();
    assert_eq!(sr_err.status_and_code(), (409, "plugin.consent_stale"));
    let sr_ok = exec_state_reset_req(
        &state,
        &detail.id,
        sr_prep.consent_token,
        sr_reauth,
        detail.revision,
        "peek-sr-ok",
    )
    .await
    .unwrap();
    assert_eq!(sr_ok.state, PluginOperationState::Succeeded);
}

#[tokio::test]
async fn execute_upgrade_blocked_drain_reconciles_to_disabled_without_wedging() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg1 = build_task_package("org.example.updrain", "1.0.0", 1, false);
    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let p1 = upload_and_install(&state, pkg1, None, grants.clone()).await;
    enable_plugin_ok(&state, "org.example.updrain", p1.revision, "en-updrain-1").await;
    let p1_en = state
        .plugins()
        .get_plugin(&state, "org.example.updrain")
        .await
        .unwrap();

    let pkg2 = build_task_package("org.example.updrain", "1.1.0", 1, false);
    let u2 = upload_pkg(&state, pkg2, None).await.unwrap();
    let prep = prepare_upgrade_ok(
        &state,
        "org.example.updrain",
        "1.1.0",
        (Some(u2.upload_id), None),
        ("preserve", "ok", p1_en.revision, "updrain-p"),
    )
    .await;

    let unk_id = plugin_p1c_support::plant_unknown_schedule_cmd(
        &state,
        "org.example.updrain",
        actor,
        "unk-updrain",
    );

    let err = exec_upgrade_req(
        &state,
        "org.example.updrain",
        prep,
        grants,
        ("preserve", None, p1_en.revision, "updrain-e"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    reconcile_schedule_cmd(&state, unk_id);
    reconcile_pending(&state, 100).unwrap();

    let after = state
        .plugins()
        .get_plugin(&state, "org.example.updrain")
        .await
        .unwrap();
    assert_eq!(after.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(after.active_operation_id, None);
    let ops = state
        .store()
        .list_plugin_operations_admin(Some("org.example.updrain"), None, None, Some(20))
        .unwrap();
    assert!(
        ops.items
            .iter()
            .all(|o| o.state != PluginOperationState::Running)
    );
    enable_plugin_ok(
        &state,
        "org.example.updrain",
        after.revision,
        "en-updrain-2",
    )
    .await;
}

#[tokio::test]
async fn pre_drain_blocked_execute_upgrade_reset_retries_with_preserved_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let pkg1 = build_task_package("org.example.pdreset", "1.0.0", 1, false);
    let p1 = upload_and_install(&state, pkg1, None, grants.clone()).await;
    enable_plugin_ok(&state, &p1.id, p1.revision, "en-pdreset-1").await;
    let p1_en = state.plugins().get_plugin(&state, &p1.id).await.unwrap();

    let pkg2 = build_task_package("org.example.pdreset", "1.1.0", 2, false);
    let u2 = upload_pkg(&state, pkg2, None).await.unwrap();
    let prep = prepare_upgrade_ok(
        &state,
        &p1.id,
        "1.1.0",
        (Some(u2.upload_id), None),
        ("reset", "v2", p1_en.revision, "pdreset-p"),
    )
    .await;
    let reauth = issue_reauth(&state, ReauthPurpose::PluginUpgrade).await;
    let unk =
        plugin_p1c_support::plant_unknown_schedule_cmd(&state, &p1.id, admin_id(), "unk-pdres");
    let err = exec_upgrade_req(
        &state,
        &p1.id,
        prep.clone(),
        grants.clone(),
        ("reset", Some(reauth.clone()), p1_en.revision, "pdreset-e1"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    reconcile_schedule_cmd(&state, unk);
    reconcile_pending(&state, 100).unwrap();
    let drained = state.plugins().get_plugin(&state, &p1.id).await.unwrap();
    assert_eq!(drained.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(drained.revision, p1_en.revision + 2);

    let old_err = exec_upgrade_req(
        &state,
        &p1.id,
        prep.clone(),
        grants.clone(),
        ("reset", Some(reauth.clone()), p1_en.revision, "pdreset-old"),
    )
    .await
    .unwrap_err();
    assert_eq!(old_err.status_and_code(), (409, "plugin.revision_conflict"));

    let ok = exec_upgrade_req(
        &state,
        &p1.id,
        prep,
        grants,
        ("reset", Some(reauth), drained.revision, "pdreset-e2"),
    )
    .await
    .unwrap();
    assert_eq!(ok.state, PluginOperationState::Succeeded);
    assert_eq!(
        state
            .plugins()
            .get_plugin(&state, &p1.id)
            .await
            .unwrap()
            .version,
        "1.1.0"
    );
}

#[tokio::test]
async fn pre_drain_blocked_rollback_retries_with_preserved_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let pkg1 = build_task_package("org.example.pdrb", "1.0.0", 1, false);
    let p1 = upload_and_install(&state, pkg1, None, grants.clone()).await;
    let pkg2 = build_task_package("org.example.pdrb", "1.1.0", 2, false);
    let u2 = upload_pkg(&state, pkg2, None).await.unwrap();
    let up_prep = prepare_upgrade_ok(
        &state,
        &p1.id,
        "1.1.0",
        (Some(u2.upload_id), None),
        ("reset", "v2", p1.revision, "pdrb-up-p"),
    )
    .await;
    let up_reauth = issue_reauth(&state, ReauthPurpose::PluginUpgrade).await;
    exec_upgrade_req(
        &state,
        &p1.id,
        up_prep,
        grants.clone(),
        ("reset", Some(up_reauth), p1.revision, "pdrb-up-e"),
    )
    .await
    .unwrap();
    let p2 = state.plugins().get_plugin(&state, &p1.id).await.unwrap();
    enable_plugin_ok(&state, &p1.id, p2.revision, "en-pdrb").await;
    let p2_en = state.plugins().get_plugin(&state, &p1.id).await.unwrap();

    let rb_prep = prepare_upgrade_ok(
        &state,
        &p1.id,
        "1.0.0",
        (None, Some(p1.package_hash.clone())),
        ("reset", "v1", p2_en.revision, "pdrb-p"),
    )
    .await;
    let rb_reauth = issue_reauth(&state, ReauthPurpose::PluginRollback).await;
    let unk =
        plugin_p1c_support::plant_unknown_schedule_cmd(&state, &p1.id, admin_id(), "unk-pdrb");
    let err = exec_rollback_req(
        &state,
        &p1.id,
        &p1.package_hash,
        rb_prep.clone(),
        grants.clone(),
        ("reset", Some(rb_reauth.clone()), p2_en.revision, "pdrb-e1"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    reconcile_schedule_cmd(&state, unk);
    reconcile_pending(&state, 200).unwrap();
    let drained = state.plugins().get_plugin(&state, &p1.id).await.unwrap();
    assert_eq!(drained.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(drained.revision, p2_en.revision + 2);

    let ok = exec_rollback_req(
        &state,
        &p1.id,
        &p1.package_hash,
        rb_prep,
        grants.clone(),
        ("reset", Some(rb_reauth), drained.revision, "pdrb-e2"),
    )
    .await
    .unwrap();
    assert_eq!(ok.state, PluginOperationState::Succeeded);
    let after = state.plugins().get_plugin(&state, &p1.id).await.unwrap();
    assert_eq!(after.package_hash, p1.package_hash);

    // Unrelated manual disable after prepare_upgrade must still reject with consent_stale
    enable_plugin_ok(&state, &p1.id, after.revision, "en-pdrb-stale").await;
    let p_en2 = state.plugins().get_plugin(&state, &p1.id).await.unwrap();
    let pkg3 = build_task_package("org.example.pdrb", "1.2.0", 1, false);
    let u3 = upload_pkg(&state, pkg3, None).await.unwrap();
    let stale_prep = prepare_upgrade_ok(
        &state,
        &p1.id,
        "1.2.0",
        (Some(u3.upload_id), None),
        ("preserve", "ok", p_en2.revision, "pdrb-stale-p"),
    )
    .await;
    let p_dis2 = disable_plugin_ok(&state, &p1.id, p_en2.revision, "pdrb-stale-dis").await;
    let stale_err = exec_upgrade_req(
        &state,
        &p1.id,
        stale_prep,
        grants,
        ("preserve", None, p_dis2.revision, "pdrb-stale-e"),
    )
    .await
    .unwrap_err();
    assert_eq!(stale_err.status_and_code(), (409, "plugin.consent_stale"));
}
