#[path = "plugin_p1c_support/mod.rs"]
mod plugin_p1c_support;

use std::collections::BTreeMap;

use api::plugins::*;
use plugin_p1c_support::pkg::{
    TestSigningKey, build_task_package, build_task_package_custom,
    build_task_package_with_schema_path,
};
use plugin_p1c_support::{
    ADMIN_PASS, admin_id, build_state, enable_dev_mode, enable_plugin_ok, install_from_upload,
    issue_reauth, pin_trust_key, req_ctx, revoke_trust_key_req, upload_and_install, upload_pkg,
};
use plugin_package::SecretWriteAction;
use store::plugins::{PluginGrant, PluginGrantScope, PluginLifecycle, PluginOperationState};

#[tokio::test]
async fn staged_install_trust_key_signature_mapping_and_package_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    let pkg = build_task_package("org.example.signed", "1.0.0", 1, false);

    let reauth = issue_reauth(&state, ReauthPurpose::PluginUpload).await;
    let (uk, ur) = req_ctx("POST", "/plugin-uploads", "up-unsigned-1", 1);
    let err = state
        .plugins()
        .stage_upload(
            &state,
            StageUploadInput {
                actor_user_id: actor,
                upload_id: uuid::Uuid::new_v4(),
                cmplugin_bytes: pkg.clone(),
                signature_text: None,
                expected_sha256: None,
                reauth_token: reauth,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.status_and_code(), (403, "plugin.signature_required"));

    let key = TestSigningKey::generate(0xDEAD_BEEF_1234_5678);
    let key_view = pin_trust_key(&state, &key).await;
    let sig = key.sign_package(&pkg);
    let grants = vec![
        PluginGrant {
            capability: "kv".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "blob".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
    ];
    let detail = upload_and_install(&state, pkg, Some(sig), grants).await;
    assert_eq!(detail.lifecycle, PluginLifecycle::InstalledDisabled);
    assert!(!detail.enabled);

    let version = state
        .store()
        .get_plugin_version("org.example.signed", &detail.package_hash)
        .unwrap()
        .unwrap();
    assert_eq!(version.signature_status, "signed");
    assert_eq!(
        version.signing_key_id.as_deref(),
        Some(key_view.key_id.as_str())
    );

    enable_plugin_ok(&state, "org.example.signed", detail.revision, "en-1").await;
    let after = state
        .plugins()
        .get_plugin(&state, "org.example.signed")
        .await
        .unwrap();
    assert_eq!(after.lifecycle, PluginLifecycle::Enabled);
    assert!(after.enabled);

    let pkg_mutated = build_task_package("org.example.signed", "1.0.0", 2, false);
    let sig_mutated = key.sign_package(&pkg_mutated);
    let upload2 = upload_pkg(&state, pkg_mutated, Some(sig_mutated))
        .await
        .unwrap();
    let (pk, pr) = req_ctx(
        "POST",
        "/plugins/org.example.signed/upgrades/prepare",
        "up-conf",
        4,
    );
    let conflict_err = state
        .plugins()
        .prepare_upgrade(
            &state,
            "org.example.signed",
            PrepareUpgradeInput {
                actor_user_id: actor,
                version: "1.0.0".into(),
                upload_id: Some(upload2.upload_id),
                package_hash: None,
                repo_id: None,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: state
                        .plugins()
                        .get_config_schema(&state, "org.example.signed")
                        .await
                        .unwrap()
                        .schema_revision,
                    values: serde_json::json!({"mode": "ok"}),
                    secrets: BTreeMap::new(),
                },
                site_id: None,
                state_policy: "reset".into(),
                reset_scopes: None,
                expected_revision: after.revision,
                idempotency_key: pk,
                request: pr,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        conflict_err.status_and_code(),
        (409, "plugin.package_conflict")
    );
}

#[tokio::test]
async fn config_update_secrets_redaction_and_not_drained_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    let key = TestSigningKey::generate(42);
    pin_trust_key(&state, &key).await;

    let pkg = build_task_package("org.example.cfg", "1.0.0", 1, true);
    let sig = key.sign_package(&pkg);
    let detail = upload_and_install(&state, pkg, Some(sig), vec![]).await;
    let schema = state
        .plugins()
        .get_config_schema(&state, "org.example.cfg")
        .await
        .unwrap();

    let invalid = state
        .plugins()
        .validate_config(
            &state,
            "org.example.cfg",
            ValidateConfigInput {
                schema_revision: schema.schema_revision.clone(),
                values: serde_json::json!({"mode": "ok"}),
                secrets: BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.status_and_code().0, 400);

    let mut secrets = BTreeMap::new();
    secrets.insert(
        "api_secret".into(),
        SecretWriteAction::Replace("super-secret-value".into()),
    );
    let (ck1, cr1) = req_ctx("PUT", "/plugins/org.example.cfg/config", "cfg-1", 5);
    let updated = state
        .plugins()
        .update_config(
            &state,
            "org.example.cfg",
            UpdateConfigInput {
                actor_user_id: actor,
                schema_revision: schema.schema_revision.clone(),
                values: serde_json::json!({"mode": "prod"}),
                secrets,
                expected_revision: detail.revision,
                site_id: None,
                idempotency_key: ck1,
                request: cr1,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.values, serde_json::json!({"mode": "prod"}));
    assert!(updated.secrets.get("api_secret").unwrap().configured);
    assert!(!format!("{updated:?}").contains("super-secret-value"));

    let mut keep = BTreeMap::new();
    keep.insert("api_secret".into(), SecretWriteAction::Keep);
    let (ck2, cr2) = req_ctx("PUT", "/plugins/org.example.cfg/config", "cfg-2", 6);
    let updated2 = state
        .plugins()
        .update_config(
            &state,
            "org.example.cfg",
            UpdateConfigInput {
                actor_user_id: actor,
                schema_revision: schema.schema_revision.clone(),
                values: serde_json::json!({"mode": "staging"}),
                secrets: keep,
                expected_revision: updated.revision,
                site_id: None,
                idempotency_key: ck2,
                request: cr2,
            },
        )
        .await
        .unwrap();
    assert!(updated2.secrets.get("api_secret").unwrap().configured);

    enable_plugin_ok(&state, "org.example.cfg", updated2.revision, "en-cfg").await;
    let enabled = state
        .plugins()
        .get_plugin(&state, "org.example.cfg")
        .await
        .unwrap();
    let (ck3, cr3) = req_ctx("PUT", "/plugins/org.example.cfg/config", "cfg-3", 7);
    let err = state
        .plugins()
        .update_config(
            &state,
            "org.example.cfg",
            UpdateConfigInput {
                actor_user_id: actor,
                schema_revision: schema.schema_revision,
                values: serde_json::json!({"mode": "nope"}),
                secrets: BTreeMap::new(),
                expected_revision: enabled.revision,
                site_id: None,
                idempotency_key: ck3,
                request: cr3,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));
}

#[tokio::test]
async fn prepare_and_execute_install_idempotency_replay_after_consent_and_expiry() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.ideminst", "1.0.0", 1, false);
    let upload = upload_pkg(&state, pkg, None).await.unwrap();
    let (pk, pr) = req_ctx("POST", "/plugin-installations/prepare", "idem-prep-1", 10);
    let prep_in = PrepareInstallInput {
        actor_user_id: actor,
        upload_id: Some(upload.upload_id),
        repo_id: None,
        plugin_id: None,
        version: None,
        idempotency_key: pk.clone(),
        request: pr.clone(),
    };
    let prep_op = state
        .plugins()
        .prepare_install(&state, prep_in.clone())
        .await
        .unwrap();
    let prep: InstallationPreparationView =
        serde_json::from_value(prep_op.result.clone().unwrap()).unwrap();

    let (ik, ir) = req_ctx("POST", "/plugin-installations", "idem-inst-1", 11);
    let exec_in = ExecuteInstallInput {
        actor_user_id: actor,
        preparation_id: prep.preparation_id,
        consent_token: prep.consent_token.clone(),
        grants: vec![],
        expected_revision: 0,
        idempotency_key: ik.clone(),
        request: ir.clone(),
    };
    let inst1 = state
        .plugins()
        .execute_install(&state, exec_in.clone())
        .await
        .unwrap();
    assert_eq!(inst1.state, PluginOperationState::Succeeded);

    // Delete upload directory to prove replay does not re-read staged upload or re-consume consent
    let upload_dir = state
        .store()
        .data_dir()
        .join("plugins")
        .join("uploads")
        .join(upload.upload_id.to_string());
    let _ = std::fs::remove_dir_all(&upload_dir);

    let prep_replay = state
        .plugins()
        .prepare_install(&state, prep_in)
        .await
        .unwrap();
    assert_eq!(prep_replay.operation_id, prep_op.operation_id);

    let inst_replay = state
        .plugins()
        .execute_install(&state, exec_in)
        .await
        .unwrap();
    assert_eq!(inst_replay.operation_id, inst1.operation_id);

    let mut conflict_in = PrepareInstallInput {
        actor_user_id: actor,
        upload_id: Some(upload.upload_id),
        repo_id: None,
        plugin_id: None,
        version: None,
        idempotency_key: ik,
        request: ir,
    };
    conflict_in.request.body_digest = [88; 32];
    let conflict = state
        .plugins()
        .prepare_install(&state, conflict_in)
        .await
        .unwrap_err();
    assert_eq!(
        conflict.status_and_code(),
        (409, "plugin.idempotency_conflict")
    );
}

#[tokio::test]
async fn enable_plugin_lifecycle_validate_allows_config_get_and_log_write() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package_custom(
        "org.example.lcvalidate",
        "1.0.0",
        1,
        false,
        Some("assert-default-mode-ok"),
        false,
        &["kv"],
    );
    let detail = upload_and_install(&state, pkg, None, vec![]).await;
    // Do NOT call update_config: snapshot.config is None, so enable_plugin must materialize default mode="ok"
    let op = enable_plugin_ok(&state, &detail.id, detail.revision, "en-lc-ok").await;
    assert_eq!(op.state, PluginOperationState::Succeeded);
    let after = state
        .plugins()
        .get_plugin(&state, &detail.id)
        .await
        .unwrap();
    assert_eq!(after.lifecycle, PluginLifecycle::Enabled);
}

#[tokio::test]
async fn enable_plugin_lifecycle_validate_rejects_state_write_with_wrong_context() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    for desc in ["wrong-context-kv-put", "wrong-context-submit"] {
        let pid = format!("org.example.wc.{desc}");
        let pkg = build_task_package_custom(&pid, "1.0.0", 1, false, Some(desc), false, &["kv"]);
        let detail = upload_and_install(&state, pkg, None, vec![]).await;
        let (ek, er) = req_ctx(
            "POST",
            &format!("/plugins/{pid}/enable"),
            &format!("en-{desc}"),
            1,
        );
        let op = state
            .plugins()
            .enable_plugin(
                &state,
                &pid,
                EnablePluginInput {
                    actor_user_id: admin_id(),
                    expected_revision: detail.revision,
                    idempotency_key: ek,
                    request: er,
                },
            )
            .await
            .unwrap();
        assert_eq!(op.state, PluginOperationState::Failed);
        assert_eq!(op.phase, "validation-rejected");
        let after = state.plugins().get_plugin(&state, &pid).await.unwrap();
        assert_eq!(after.lifecycle, PluginLifecycle::Disabled);
    }
}

#[tokio::test]
async fn persistent_idempotency_replay_for_dev_mode_and_trust_key() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let dev_reauth = issue_reauth(&state, ReauthPurpose::DeveloperModeEnable).await;
    let (dk, dr) = req_ctx("POST", "/settings/plugins/developer/enable", "idem-dev", 20);
    let dev_in = EnableDeveloperModeInput {
        reauth_token: dev_reauth.clone(),
        expected_revision: 0,
        idempotency_key: dk,
        request: dr,
    };
    let dev1 = state
        .plugins()
        .enable_developer_mode(&state, actor, dev_in.clone())
        .await
        .unwrap();
    let dev2 = state
        .plugins()
        .enable_developer_mode(&state, actor, dev_in)
        .await
        .unwrap();
    assert_eq!(dev1, dev2);

    let key = TestSigningKey::generate(555);
    let prep = state
        .plugins()
        .prepare_trust_key(
            &state,
            actor,
            PrepareTrustKeyInput {
                public_key: key.public_key_str.clone(),
                label: "Idem Key".into(),
            },
        )
        .await
        .unwrap();
    let tk_reauth = issue_reauth(&state, ReauthPurpose::TrustKeyCreate).await;
    let (tk_k, tk_r) = req_ctx("POST", "/plugin-trust-keys", "idem-tk", 21);
    let tk_in = CreateTrustKeyInput {
        label: Some("Idem Key".into()),
        public_key: prep.public_key,
        fingerprint: prep.fingerprint,
        consent_token: prep.consent_token,
        reauth_token: tk_reauth.clone(),
        expected_revision: 0,
        idempotency_key: tk_k,
        request: tk_r,
    };
    let tk1 = state
        .plugins()
        .create_trust_key(&state, actor, tk_in.clone())
        .await
        .unwrap();
    let tk2 = state
        .plugins()
        .create_trust_key(&state, actor, tk_in)
        .await
        .unwrap();
    assert_eq!(tk1, tk2);

    let raw_settings = read_idem_settings(&state);
    assert!(raw_settings.len() >= 2);
    for raw in &raw_settings {
        assert!(!raw.contains(&dev_reauth));
        assert!(!raw.contains(&tk_reauth));
    }
    let _ = ADMIN_PASS;
}

#[tokio::test]
async fn persistent_idempotency_replay_and_redaction_for_upload_and_config() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.idem6", "1.0.0", 1, true);
    let up_reauth = issue_reauth(&state, ReauthPurpose::PluginUpload).await;
    let (up_k, up_r) = req_ctx("POST", "/plugin-uploads", "idem-up", 22);
    let up_in = StageUploadInput {
        actor_user_id: actor,
        upload_id: uuid::Uuid::new_v4(),
        cmplugin_bytes: pkg.clone(),
        signature_text: None,
        expected_sha256: None,
        reauth_token: up_reauth.clone(),
        idempotency_key: up_k,
        request: up_r,
    };
    let up1 = state
        .plugins()
        .stage_upload(&state, up_in.clone())
        .await
        .unwrap();
    let up2 = state.plugins().stage_upload(&state, up_in).await.unwrap();
    assert_eq!(up1, up2);

    let installed = upload_and_install(&state, pkg, None, vec![]).await;
    let schema = state
        .plugins()
        .get_config_schema(&state, &installed.id)
        .await
        .unwrap();
    let mut secrets = BTreeMap::new();
    secrets.insert(
        "api_secret".into(),
        SecretWriteAction::Replace("never-persist-in-idem".into()),
    );
    let (ck, cr) = req_ctx("PUT", "/plugins/org.example.idem6/config", "idem-cfg", 23);
    let cfg_in = UpdateConfigInput {
        actor_user_id: actor,
        schema_revision: schema.schema_revision,
        values: serde_json::json!({"mode": "prod"}),
        secrets,
        expected_revision: installed.revision,
        site_id: None,
        idempotency_key: ck,
        request: cr,
    };
    let cfg1 = state
        .plugins()
        .update_config(&state, &installed.id, cfg_in.clone())
        .await
        .unwrap();
    enable_plugin_ok(&state, &installed.id, cfg1.revision, "en-idem6").await;
    let cfg2 = state
        .plugins()
        .update_config(&state, &installed.id, cfg_in)
        .await
        .unwrap();
    assert_eq!(cfg1, cfg2);

    for raw in &read_idem_settings(&state) {
        assert!(!raw.contains("never-persist-in-idem"));
        assert!(!raw.contains(&up_reauth));
    }
}

fn read_idem_settings(state: &api::ApiState) -> Vec<String> {
    let conn = rusqlite::Connection::open(state.store().data_dir().join("app.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT value FROM settings WHERE key LIKE 'plugin_idem.%'")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn execute_install_rejects_missing_site_grant_before_consuming_consent() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.badsite", "1.0.0", 1, false);
    let upload = upload_pkg(&state, pkg, None).await.unwrap();
    let (pk, pr) = req_ctx("POST", "/plugin-installations/prepare", "p-bs", 1);
    let prep_op = state
        .plugins()
        .prepare_install(
            &state,
            PrepareInstallInput {
                actor_user_id: admin_id(),
                upload_id: Some(upload.upload_id),
                repo_id: None,
                plugin_id: None,
                version: None,
                idempotency_key: pk,
                request: pr,
            },
        )
        .await
        .unwrap();
    let prep: InstallationPreparationView =
        serde_json::from_value(prep_op.result.unwrap()).unwrap();

    let (ik, ir) = req_ctx("POST", "/plugin-installations", "i-bs-bad", 2);
    let bad_err = state
        .plugins()
        .execute_install(
            &state,
            ExecuteInstallInput {
                actor_user_id: admin_id(),
                preparation_id: prep.preparation_id,
                consent_token: prep.consent_token.clone(),
                grants: vec![PluginGrant {
                    capability: "kv".into(),
                    scope: PluginGrantScope::Site(domain::SiteId::new()),
                    sensitive: false,
                }],
                expected_revision: 0,
                idempotency_key: ik,
                request: ir,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad_err.status_and_code(), (400, "plugin.scope_invalid"));

    // Consent token must still be unconsumed and usable with valid global grant
    let (ik2, ir2) = req_ctx("POST", "/plugin-installations", "i-bs-ok", 3);
    let ok = state
        .plugins()
        .execute_install(
            &state,
            ExecuteInstallInput {
                actor_user_id: admin_id(),
                preparation_id: prep.preparation_id,
                consent_token: prep.consent_token,
                grants: vec![PluginGrant {
                    capability: "kv".into(),
                    scope: PluginGrantScope::Global,
                    sensitive: false,
                }],
                expected_revision: 0,
                idempotency_key: ik2,
                request: ir2,
            },
        )
        .await
        .unwrap();
    assert_eq!(ok.state, PluginOperationState::Succeeded);
}

#[tokio::test]
async fn precheck_preserves_reauth_on_enable_dev_mode_and_revoke_trust_key() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let dev_reauth = issue_reauth(&state, ReauthPurpose::DeveloperModeEnable).await;
    let (dk1, dr1) = req_ctx("POST", "/settings/plugins/developer/enable", "dev-bad", 91);
    assert_eq!(
        state
            .plugins()
            .enable_developer_mode(
                &state,
                actor,
                EnableDeveloperModeInput {
                    reauth_token: dev_reauth.clone(),
                    expected_revision: 99,
                    idempotency_key: dk1,
                    request: dr1,
                },
            )
            .await
            .unwrap_err()
            .status_and_code(),
        (409, "plugin.revision_conflict")
    );
    let (dk2, dr2) = req_ctx("POST", "/settings/plugins/developer/enable", "dev-ok", 92);
    assert!(
        state
            .plugins()
            .enable_developer_mode(
                &state,
                actor,
                EnableDeveloperModeInput {
                    reauth_token: dev_reauth,
                    expected_revision: 0,
                    idempotency_key: dk2,
                    request: dr2,
                },
            )
            .await
            .unwrap()
            .developer_mode
    );

    let key = TestSigningKey::generate(911);
    let kv = pin_trust_key(&state, &key).await;
    let rev_reauth = issue_reauth(&state, ReauthPurpose::TrustKeyRevoke).await;
    let (rk1, rr1) = req_ctx("POST", "/plugin-trust-keys/revoke", "rev-missing", 93);
    assert_eq!(
        state
            .plugins()
            .revoke_trust_key(
                &state,
                actor,
                RevokeTrustKeyInput {
                    key_id: "nonexistent".into(),
                    reason: "test".into(),
                    reauth_token: rev_reauth.clone(),
                    expected_revision: 1,
                    idempotency_key: rk1,
                    request: rr1,
                },
            )
            .await
            .unwrap_err()
            .status_and_code()
            .0,
        404
    );
    let (rk2, rr2) = req_ctx("POST", "/plugin-trust-keys/revoke", "rev-badrev", 94);
    assert_eq!(
        state
            .plugins()
            .revoke_trust_key(
                &state,
                actor,
                RevokeTrustKeyInput {
                    key_id: kv.key_id.clone(),
                    reason: "test".into(),
                    reauth_token: rev_reauth.clone(),
                    expected_revision: 999,
                    idempotency_key: rk2,
                    request: rr2,
                },
            )
            .await
            .unwrap_err()
            .status_and_code(),
        (409, "plugin.revision_conflict")
    );
    let (rk3, rr3) = req_ctx("POST", "/plugin-trust-keys/revoke", "rev-ok", 95);
    let ok = state
        .plugins()
        .revoke_trust_key(
            &state,
            actor,
            RevokeTrustKeyInput {
                key_id: kv.key_id,
                reason: "test".into(),
                reauth_token: rev_reauth,
                expected_revision: kv.revision,
                idempotency_key: rk3,
                request: rr3,
            },
        )
        .await
        .unwrap();
    assert_eq!(ok.state, PluginOperationState::Succeeded);
}

#[tokio::test]
async fn revoke_trust_key_overlong_reason_and_reserved_schema_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let key = TestSigningKey::generate(912);
    let kv = pin_trust_key(&state, &key).await;
    let rev_reauth = issue_reauth(&state, ReauthPurpose::TrustKeyRevoke).await;
    let bad = revoke_trust_key_req(
        &state,
        &kv.key_id,
        &"x".repeat(1025),
        rev_reauth.clone(),
        kv.revision,
        "rev-long",
    )
    .await
    .unwrap_err();
    assert_eq!(bad.status_and_code(), (400, "plugin.invalid_input"));

    let ok = revoke_trust_key_req(
        &state,
        &kv.key_id,
        "ok",
        rev_reauth,
        kv.revision,
        "rev-long-ok",
    )
    .await
    .unwrap();
    assert_eq!(ok.state, PluginOperationState::Succeeded);

    let bad_pkg =
        build_task_package_with_schema_path("org.example.resv", "1.0.0", "package.cmplugin");
    let upload = upload_pkg(&state, bad_pkg, None).await.unwrap();
    let inst_err = install_from_upload(&state, upload.upload_id, vec![], "resv")
        .await
        .unwrap_err();
    assert_eq!(inst_err.status_and_code(), (400, "plugin.invalid_package"));
}
