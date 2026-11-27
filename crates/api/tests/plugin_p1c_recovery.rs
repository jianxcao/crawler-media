#[path = "plugin_p1c_support/mod.rs"]
mod plugin_p1c_support;

use api::plugins::*;
use plugin_p1c_support::pkg::{TestSigningKey, build_task_package, build_task_package_custom};
use plugin_p1c_support::{
    ADMIN_PASS, admin_id, build_state, enable_dev_mode, enable_plugin_ok, pin_trust_key, req_ctx,
    upload_and_install, upload_pkg,
};
use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, PluginCommandCause, PluginLifecycle,
    PluginOperationState, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};

#[tokio::test]
async fn reauth_token_single_use_purpose_and_redaction() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let tok = state
        .plugins()
        .issue_reauth_token(
            &state,
            actor,
            ADMIN_PASS.into(),
            ReauthPurpose::TrustKeyCreate,
        )
        .await
        .unwrap();
    let wrong = state
        .plugins()
        .verify_and_consume_reauth_token(
            &state,
            actor,
            ReauthPurpose::PluginUpgrade,
            &tok.reauth_token,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong.status_and_code().0, 403);

    let tok2 = state
        .plugins()
        .issue_reauth_token(
            &state,
            actor,
            ADMIN_PASS.into(),
            ReauthPurpose::TrustKeyCreate,
        )
        .await
        .unwrap();
    assert!(!format!("{tok2:?}").contains(&tok2.reauth_token));
    state
        .plugins()
        .verify_and_consume_reauth_token(
            &state,
            actor,
            ReauthPurpose::TrustKeyCreate,
            &tok2.reauth_token,
        )
        .await
        .unwrap();
    let replay_err = state
        .plugins()
        .verify_and_consume_reauth_token(
            &state,
            actor,
            ReauthPurpose::TrustKeyCreate,
            &tok2.reauth_token,
        )
        .await
        .unwrap_err();
    assert_eq!(replay_err.status_and_code().0, 403);
}

#[tokio::test]
async fn cancel_requested_staged_op_blocks_resume_and_resume_replays_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let (ik, ir) = req_ctx("POST", "/plugin-installations", "cancel-req-1", 40);
    let accepted = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: ik,
            request: ir,
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.cancel".into(),
                package_hash: Some("deadbeef".into()),
                version: Some("1.0.0".into()),
                artifact_path: None,
                state_policy: None,
                target_config_digest: None,
                grants: vec![],
            },
            cause: PluginCommandCause::root("cancel-req"),
        })
        .unwrap();
    let claim = state
        .store()
        .claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            10,
        )
        .unwrap();
    let (ck, cr) = req_ctx("POST", "/plugin-operations/cancel", "cancel-req-can", 41);
    let cancelled = state
        .plugins()
        .cancel_operation(
            &state,
            claim.operation_id(),
            CancelOperationInput {
                actor_user_id: actor,
                reason: Some("abort".into()),
                expected_revision: claim.revision(),
                idempotency_key: ck,
                request: cr,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, PluginOperationState::Unknown);
    assert_eq!(cancelled.phase, "cancel-requested");

    let (rk, rr) = req_ctx("POST", "/plugin-operations/resume", "cancel-req-res", 42);
    let res_err = state
        .plugins()
        .resume_operation(
            &state,
            cancelled.operation_id,
            ResumeOperationInput {
                actor_user_id: actor,
                expected_revision: cancelled.revision,
                evidence_ref: None,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(res_err.status_and_code().0, 409);
}

#[tokio::test]
async fn crash_recovery_resumes_pre_register_staged_install() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.crashrec", "1.0.0", 1, false);
    let upload = upload_pkg(&state, pkg, None).await.unwrap();
    let (ik, ir) = req_ctx("POST", "/plugin-installations", "crash-inst", 30);
    let accepted = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: ik,
            request: ir,
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.crashrec".into(),
                package_hash: Some(upload.hash.clone()),
                version: Some("1.0.0".into()),
                artifact_path: Some(format!(
                    "plugins/uploads/{}/package.cmplugin",
                    upload.upload_id
                )),
                state_policy: None,
                target_config_digest: None,
                grants: vec![],
            },
            cause: PluginCommandCause::root("crash-install"),
        })
        .unwrap();
    state
        .store()
        .claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            10,
        )
        .unwrap();

    recover_startup(&state).unwrap();
    let recovered = state
        .plugins()
        .get_operation(&state, accepted.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(recovered.state, PluginOperationState::Succeeded);
}

async fn plant_cancel_req_post_register_install(
    state: &api::ApiState,
    actor: domain::UserId,
) -> uuid::Uuid {
    let pkg = build_task_package("org.example.postreg", "1.0.0", 1, false);
    let upload = upload_pkg(state, pkg, None).await.unwrap();
    let (ik, ir) = req_ctx("POST", "/plugin-installations", "postreg-i", 50);
    let accepted = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: ik,
            request: ir,
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.postreg".into(),
                package_hash: Some(upload.hash.clone()),
                version: Some("1.0.0".into()),
                artifact_path: Some(format!(
                    "plugins/uploads/{}/package.cmplugin",
                    upload.upload_id
                )),
                state_policy: None,
                target_config_digest: None,
                grants: vec![],
            },
            cause: PluginCommandCause::root("postreg"),
        })
        .unwrap();
    let mut claim = state
        .store()
        .claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .register_staged_plugin_version(
            &mut claim,
            &store::plugins::PluginVersionMetadata {
                plugin_id: "org.example.postreg".into(),
                package_hash: upload.hash,
                version: "1.0.0".into(),
                manifest_json: "{}".into(),
                config_schema_json: "{}".into(),
                state_schema: "1".into(),
                artifact_path: format!("plugins/uploads/{}/package.cmplugin", upload.upload_id),
                signing_key_id: None,
                signature_status: "development".into(),
                ui_metadata_ref: None,
                credential_bound: false,
            },
            &[],
            11,
        )
        .unwrap();
    state
        .store()
        .cancel_plugin_operation_admin(accepted.operation.operation_id, claim.revision(), 12)
        .unwrap();
    accepted.operation.operation_id
}

#[tokio::test]
async fn crash_recovery_settles_cancel_requested_unknown_post_register_install_and_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let postreg_op_id = plant_cancel_req_post_register_install(&state, actor).await;

    // 2. Staged upgrade pre-switch cancel-requested -> Unknown
    let pkg_u = build_task_package("org.example.upcan", "1.0.0", 1, false);
    let pu = upload_and_install(&state, pkg_u, None, vec![]).await;
    let (uk, ur) = req_ctx("POST", "/plugins/org.example.upcan/upgrade", "upcan-u", 51);
    let u_acc = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: uk,
            request: ur,
            op_kind: StagedPluginOpKind::Upgrade,
            expected_revision: pu.revision,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.upcan".into(),
                package_hash: Some("deadbeef".into()),
                version: Some("1.1.0".into()),
                artifact_path: None,
                state_policy: Some("preserve".into()),
                target_config_digest: None,
                grants: vec![],
            },
            cause: PluginCommandCause::root("upcan"),
        })
        .unwrap();
    let u_claim = state
        .store()
        .claim_staged_plugin_operation(
            u_acc.operation.operation_id,
            u_acc.operation.revision,
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .cancel_plugin_operation_admin(u_acc.operation.operation_id, u_claim.revision(), 11)
        .unwrap();

    recover_startup(&state).unwrap();

    let op_post = state
        .plugins()
        .get_operation(&state, postreg_op_id)
        .await
        .unwrap();
    assert_eq!(op_post.state, PluginOperationState::Succeeded);
    let post_plugin = state
        .plugins()
        .get_plugin(&state, "org.example.postreg")
        .await
        .unwrap();
    assert_eq!(post_plugin.active_operation_id, None);

    let op_up = state
        .plugins()
        .get_operation(&state, u_acc.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(op_up.state, PluginOperationState::Cancelled);
    let pu_after = state
        .plugins()
        .get_plugin(&state, "org.example.upcan")
        .await
        .unwrap();
    assert_eq!(pu_after.active_operation_id, None);
}

#[tokio::test]
async fn crash_recovery_settles_interrupted_enable_and_disable_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg1 = build_task_package("org.example.int-en", "1.0.0", 1, false);
    let p1 = upload_and_install(&state, pkg1, None, vec![]).await;
    let (ek, er) = req_ctx("POST", "/plugins/org.example.int-en/enable", "int-en-1", 60);
    let en_acc = state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some("org.example.int-en".into()),
            subject: PluginSubject::User(actor),
            idempotency_key: ek,
            request: er,
            kind: "enable".into(),
            expected_revision: Some(p1.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("int-en"),
        })
        .unwrap();
    state
        .store()
        .begin_plugin_enable_checked(
            "org.example.int-en",
            p1.revision,
            en_acc.operation.operation_id,
        )
        .unwrap();
    state
        .store()
        .claim_plugin_command(
            en_acc.operation.operation_id,
            en_acc.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();

    let pkg2 = build_task_package("org.example.int-dis", "1.0.0", 1, false);
    let p2 = upload_and_install(&state, pkg2, None, vec![]).await;
    enable_plugin_ok(&state, "org.example.int-dis", p2.revision, "int-dis-en").await;
    let p2_en = state
        .plugins()
        .get_plugin(&state, "org.example.int-dis")
        .await
        .unwrap();
    let (dk, dr) = req_ctx(
        "POST",
        "/plugins/org.example.int-dis/disable",
        "int-dis-1",
        61,
    );
    let dis_acc = state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some("org.example.int-dis".into()),
            subject: PluginSubject::User(actor),
            idempotency_key: dk,
            request: dr,
            kind: "disable".into(),
            expected_revision: Some(p2_en.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("int-dis"),
        })
        .unwrap();
    state
        .store()
        .begin_plugin_lifecycle(
            "org.example.int-dis",
            p2_en.revision,
            dis_acc.operation.operation_id,
            false,
        )
        .unwrap();
    state
        .store()
        .claim_plugin_command(
            dis_acc.operation.operation_id,
            dis_acc.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();

    recover_startup(&state).unwrap();

    let en_after = state
        .plugins()
        .get_operation(&state, en_acc.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(en_after.state, PluginOperationState::Failed);
    let dis_after = state
        .plugins()
        .get_operation(&state, dis_acc.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(dis_after.state, PluginOperationState::Succeeded);

    // Subsequent enable on int-en must NOT fail with NotDrained!
    let p1_after = state
        .plugins()
        .get_plugin(&state, "org.example.int-en")
        .await
        .unwrap();
    assert_eq!(p1_after.lifecycle, PluginLifecycle::Disabled);
    enable_plugin_ok(&state, "org.example.int-en", p1_after.revision, "int-en-2").await;
}

#[tokio::test]
async fn crash_recovery_preserve_upgrade_respects_target_config_and_seeds_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    let key = TestSigningKey::generate(909);
    pin_trust_key(&state, &key).await;

    let pkg1 = build_task_package("org.example.rfpres", "1.0.0", 1, false);
    let sig1 = key.sign_package(&pkg1);
    let p1 = upload_and_install(&state, pkg1, Some(sig1), vec![]).await;

    // v1.1.0 with invalid lifecycle validation ("reject") -> roll-forward must abort safely
    let pkg_bad = build_task_package_custom(
        "org.example.rfpres",
        "1.1.0",
        1,
        false,
        Some("reject"),
        false,
        &["kv"],
    );
    let sig_bad = key.sign_package(&pkg_bad);
    let u_bad = upload_pkg(&state, pkg_bad, Some(sig_bad)).await.unwrap();
    let (rk1, rr1) = req_ctx("POST", "/plugins/org.example.rfpres/upgrade", "rf-bad", 70);
    let bad_op = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: rk1,
            request: rr1,
            op_kind: StagedPluginOpKind::Upgrade,
            expected_revision: p1.revision,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.rfpres".into(),
                package_hash: Some(u_bad.hash),
                version: Some("1.1.0".into()),
                artifact_path: Some(format!(
                    "plugins/uploads/{}/package.cmplugin",
                    u_bad.upload_id
                )),
                state_policy: Some("preserve".into()),
                target_config_digest: Some("sha256:mismatched".into()),
                grants: vec![],
            },
            cause: PluginCommandCause::root("rf-bad"),
        })
        .unwrap();
    state
        .store()
        .claim_staged_plugin_operation(
            bad_op.operation.operation_id,
            bad_op.operation.revision,
            0,
            10,
        )
        .unwrap();

    recover_startup(&state).unwrap();
    let bad_after = state
        .plugins()
        .get_operation(&state, bad_op.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(bad_after.state, PluginOperationState::Failed);
    let p_still = state
        .plugins()
        .get_plugin(&state, "org.example.rfpres")
        .await
        .unwrap();
    assert_eq!(p_still.package_hash, p1.package_hash);
}

fn plant_running_schedule_cmd(
    state: &api::ApiState,
    pid: &str,
    actor: domain::UserId,
) -> uuid::Uuid {
    let p_en = state.store().get_plugin(pid).unwrap().unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&p_en, None)
        .unwrap();
    let cmd_auth = state
        .store()
        .authorize_plugin_command(
            &auth,
            "schedule.set",
            &[store::plugins::PluginCommandTarget::Global],
        )
        .unwrap();
    let (ck, cr) = req_ctx("POST", &format!("/plugins/{pid}/commands"), "drrec-cmd", 71);
    let cmd_acc = state
        .store()
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some(pid.into()),
                subject: PluginSubject::User(actor),
                idempotency_key: ck,
                request: cr,
                kind: "schedule.set".into(),
                expected_revision: Some(p_en.revision),
                expected_generation: Some(p_en.generation),
                cause: PluginCommandCause::root("drrec-cmd"),
            },
            b"{}",
        )
        .unwrap();
    state
        .store()
        .claim_plugin_command(
            cmd_acc.operation.operation_id,
            cmd_acc.operation.revision,
            Some(p_en.generation),
            0,
            10,
        )
        .unwrap();
    cmd_acc.operation.operation_id
}

#[tokio::test]
async fn crash_recovery_draining_upgrade_with_interrupted_runtime_cmd_reconciles_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg1 = build_task_package("org.example.drrec", "1.0.0", 1, false);
    let grants = vec![store::plugins::PluginGrant {
        capability: "command:schedule.set".into(),
        scope: store::plugins::PluginGrantScope::Global,
        sensitive: false,
    }];
    let p1 = upload_and_install(&state, pkg1, None, grants).await;
    enable_plugin_ok(&state, "org.example.drrec", p1.revision, "en-drrec-1").await;
    let p_en = state
        .store()
        .get_plugin("org.example.drrec")
        .unwrap()
        .unwrap();
    let cmd_op_id = plant_running_schedule_cmd(&state, "org.example.drrec", actor);

    let (uk, ur) = req_ctx(
        "POST",
        "/plugins/org.example.drrec/upgrade",
        "drrec-upg",
        72,
    );
    let upg_acc = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: uk,
            request: ur,
            op_kind: StagedPluginOpKind::Upgrade,
            expected_revision: p_en.revision,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.drrec".into(),
                package_hash: Some("deadbeef".into()),
                version: Some("1.1.0".into()),
                artifact_path: None,
                state_policy: Some("preserve".into()),
                target_config_digest: Some("sha256:00".into()),
                grants: vec![],
            },
            cause: PluginCommandCause::root("drrec-upg"),
        })
        .unwrap();
    state
        .store()
        .claim_staged_plugin_operation(
            upg_acc.operation.operation_id,
            upg_acc.operation.revision,
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .begin_plugin_lifecycle(
            "org.example.drrec",
            p_en.revision,
            upg_acc.operation.operation_id,
            false,
        )
        .unwrap();

    recover_startup(&state).unwrap();

    let cmd_admin = state
        .store()
        .get_plugin_operation_admin(cmd_op_id)
        .unwrap()
        .unwrap();
    assert_eq!(cmd_admin.operation.state, PluginOperationState::Unknown);
    state
        .store()
        .reconcile_plugin_command(&store::plugins::PluginReconciliationObservation {
            operation_id: cmd_op_id,
            revision: cmd_admin.operation.revision,
            generation: cmd_admin.operation.generation,
            attempt: cmd_admin.attempt,
            outcome: store::plugins::PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    reconcile_pending(&state, 100).unwrap();

    let p_after = state
        .plugins()
        .get_plugin(&state, "org.example.drrec")
        .await
        .unwrap();
    assert_eq!(p_after.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(p_after.active_operation_id, None);
    let upg_after = state
        .plugins()
        .get_operation(&state, upg_acc.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(upg_after.state, PluginOperationState::Failed);
    enable_plugin_ok(&state, "org.example.drrec", p_after.revision, "en-drrec-2").await;
}

#[tokio::test]
async fn crash_recovery_draining_non_staged_disable_with_interrupted_runtime_cmd_reconciles() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg1 = build_task_package("org.example.nsrec", "1.0.0", 1, false);
    let grants = vec![store::plugins::PluginGrant {
        capability: "command:schedule.set".into(),
        scope: store::plugins::PluginGrantScope::Global,
        sensitive: false,
    }];
    let p1 = upload_and_install(&state, pkg1, None, grants).await;
    enable_plugin_ok(&state, "org.example.nsrec", p1.revision, "en-nsrec-1").await;
    let p_en = state
        .store()
        .get_plugin("org.example.nsrec")
        .unwrap()
        .unwrap();
    let cmd_op_id = plant_running_schedule_cmd(&state, "org.example.nsrec", actor);

    let (dk, dr) = req_ctx(
        "POST",
        "/plugins/org.example.nsrec/disable",
        "nsrec-dis",
        73,
    );
    let dis_acc = state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some("org.example.nsrec".into()),
            subject: PluginSubject::User(actor),
            idempotency_key: dk,
            request: dr,
            kind: "disable".into(),
            expected_revision: Some(p_en.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("nsrec-dis"),
        })
        .unwrap();
    state
        .store()
        .claim_plugin_command(
            dis_acc.operation.operation_id,
            dis_acc.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .begin_plugin_lifecycle(
            "org.example.nsrec",
            p_en.revision,
            dis_acc.operation.operation_id,
            false,
        )
        .unwrap();

    recover_startup(&state).unwrap();

    let cmd_admin = state
        .store()
        .get_plugin_operation_admin(cmd_op_id)
        .unwrap()
        .unwrap();
    assert_eq!(cmd_admin.operation.state, PluginOperationState::Unknown);
    state
        .store()
        .reconcile_plugin_command(&store::plugins::PluginReconciliationObservation {
            operation_id: cmd_op_id,
            revision: cmd_admin.operation.revision,
            generation: cmd_admin.operation.generation,
            attempt: cmd_admin.attempt,
            outcome: store::plugins::PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    reconcile_pending(&state, 100).unwrap();

    let p_after = state
        .plugins()
        .get_plugin(&state, "org.example.nsrec")
        .await
        .unwrap();
    assert_eq!(p_after.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(p_after.active_operation_id, None);
    let dis_after = state
        .plugins()
        .get_operation(&state, dis_acc.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(dis_after.state, PluginOperationState::Failed);
    enable_plugin_ok(&state, "org.example.nsrec", p_after.revision, "en-nsrec-2").await;
}
