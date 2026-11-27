#[path = "plugin_p1c_support/mod.rs"]
mod plugin_p1c_support;

use api::plugins::*;
use plugin_p1c_support::pkg::{TestSigningKey, build_task_package};
use plugin_p1c_support::{
    admin_id, build_state, enable_dev_mode, enable_plugin_ok, issue_reauth, pin_trust_key, req_ctx,
    upload_and_install,
};
use store::plugins::{
    AcceptPluginOperation, PluginCommandCause, PluginGrant, PluginGrantScope, PluginLifecycle,
    PluginOperationState, PluginStateKind, PluginSubject,
};

#[tokio::test]
async fn revoke_trust_key_drains_enabled_plugin_and_uninstall_cleans_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let key = TestSigningKey::generate(777);
    let key_view = pin_trust_key(&state, &key).await;
    let pkg = build_task_package("org.example.cascade", "1.0.0", 1, false);
    let sig = key.sign_package(&pkg);
    let installed = upload_and_install(&state, pkg, Some(sig), vec![]).await;
    enable_plugin_ok(&state, "org.example.cascade", installed.revision, "en-cas").await;

    let reauth = issue_reauth(&state, ReauthPurpose::TrustKeyRevoke).await;
    let (rk, rr) = req_ctx("POST", "/plugin-trust-keys/revoke", "rev-k", 21);
    let rev_op = state
        .plugins()
        .revoke_trust_key(
            &state,
            actor,
            RevokeTrustKeyInput {
                key_id: key_view.key_id,
                reason: "compromised".into(),
                reauth_token: reauth,
                expected_revision: key_view.revision,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
        .unwrap();
    assert_eq!(rev_op.state, PluginOperationState::Succeeded);

    let after_revoke = state
        .plugins()
        .get_plugin(&state, "org.example.cascade")
        .await
        .unwrap();
    assert_eq!(after_revoke.lifecycle, PluginLifecycle::Disabled);
    assert!(!after_revoke.enabled);

    let pkg_dir = state
        .store()
        .data_dir()
        .join("plugins")
        .join("packages")
        .join(&after_revoke.package_hash);
    assert!(pkg_dir.exists());

    let (uk, ur) = req_ctx("POST", "/plugins/org.example.cascade/uninstall", "un-1", 22);
    let un_op = state
        .plugins()
        .uninstall_plugin(
            &state,
            "org.example.cascade",
            UninstallPluginInput {
                actor_user_id: actor,
                remove_private_data: true,
                expected_revision: after_revoke.revision,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap();
    assert_eq!(un_op.state, PluginOperationState::Succeeded);
    assert!(!pkg_dir.exists());
    assert!(
        state
            .store()
            .get_plugin("org.example.cascade")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn disable_developer_mode_drains_unsigned_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.devdis", "1.0.0", 1, false);
    let installed = upload_and_install(&state, pkg, None, vec![]).await;
    enable_plugin_ok(&state, "org.example.devdis", installed.revision, "en-dd").await;

    let settings = state.plugins().get_plugin_settings(&state).await.unwrap();
    let (dk, dr) = req_ctx("POST", "/settings/plugins/developer/disable", "dev-dis", 16);
    let dis_op = state
        .plugins()
        .disable_developer_mode(
            &state,
            admin_id(),
            DisableDeveloperModeInput {
                expected_revision: settings.revision,
                idempotency_key: dk,
                request: dr,
            },
        )
        .await
        .unwrap();
    assert_eq!(dis_op.state, PluginOperationState::Succeeded);
    let p_final = state
        .plugins()
        .get_plugin(&state, "org.example.devdis")
        .await
        .unwrap();
    assert_eq!(p_final.lifecycle, PluginLifecycle::Disabled);
}

#[tokio::test]
async fn revoke_trust_key_aborts_enabling_plugin_and_settles_enable_op_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let key = TestSigningKey::generate(888);
    let key_view = pin_trust_key(&state, &key).await;
    let pkg = build_task_package("org.example.enabort", "1.0.0", 1, false);
    let sig = key.sign_package(&pkg);
    let installed = upload_and_install(&state, pkg, Some(sig), vec![]).await;

    // Simulate a plugin mid-way through enable_plugin (`Enabling` + active `enable` op)
    let (ek, er) = req_ctx(
        "POST",
        "/plugins/org.example.enabort/enable",
        "en-abort-1",
        30,
    );
    let accepted = state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some("org.example.enabort".into()),
            subject: PluginSubject::User(actor),
            idempotency_key: ek,
            request: er,
            kind: "enable".into(),
            expected_revision: Some(installed.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("enable:org.example.enabort"),
        })
        .unwrap();
    let _snap = state
        .store()
        .begin_plugin_enable_checked(
            "org.example.enabort",
            installed.revision,
            accepted.operation.operation_id,
        )
        .unwrap();
    state
        .store()
        .claim_plugin_command(
            accepted.operation.operation_id,
            accepted.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();

    let reauth = issue_reauth(&state, ReauthPurpose::TrustKeyRevoke).await;
    let (rk, rr) = req_ctx("POST", "/plugin-trust-keys/revoke", "rev-abort", 31);
    let rev_op = state
        .plugins()
        .revoke_trust_key(
            &state,
            actor,
            RevokeTrustKeyInput {
                key_id: key_view.key_id,
                reason: "revoked-during-enable".into(),
                reauth_token: reauth,
                expected_revision: key_view.revision,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
        .unwrap();
    assert_eq!(rev_op.state, PluginOperationState::Succeeded);

    let enable_after = state
        .plugins()
        .get_operation(&state, accepted.operation.operation_id)
        .await
        .unwrap();
    assert_eq!(enable_after.state, PluginOperationState::Failed);
    let p_after = state
        .plugins()
        .get_plugin(&state, "org.example.enabort")
        .await
        .unwrap();
    assert_eq!(p_after.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(p_after.active_operation_id, None);
}

#[tokio::test]
async fn cascade_drain_continues_across_plugins_when_one_fails_to_drain() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg_a = build_task_package("org.example.cas-a", "1.0.0", 1, false);
    let pkg_b = build_task_package("org.example.cas-b", "1.0.0", 1, false);
    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let pa = upload_and_install(&state, pkg_a, None, grants).await;
    let pb = upload_and_install(&state, pkg_b, None, vec![]).await;
    enable_plugin_ok(&state, "org.example.cas-a", pa.revision, "en-ca").await;
    enable_plugin_ok(&state, "org.example.cas-b", pb.revision, "en-cb").await;

    // Plant an Unknown runtime op on cas-a so cas-a fails drain_plugin_sync with NotDrained
    let pa_en = state
        .store()
        .get_plugin("org.example.cas-a")
        .unwrap()
        .unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&pa_en, None)
        .unwrap();
    let cmd_auth = state
        .store()
        .authorize_plugin_command(
            &auth,
            "schedule.set",
            &[store::plugins::PluginCommandTarget::Global],
        )
        .unwrap();
    let (ck, cr) = req_ctx("POST", "/plugins/org.example.cas-a/commands", "unk-ca", 32);
    let unk = state
        .store()
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some("org.example.cas-a".into()),
                subject: PluginSubject::User(actor),
                idempotency_key: ck,
                request: cr,
                kind: "schedule.set".into(),
                expected_revision: Some(pa_en.revision),
                expected_generation: Some(pa_en.generation),
                cause: PluginCommandCause::root("unk-ca"),
            },
            b"{}",
        )
        .unwrap();
    let claim = state
        .store()
        .claim_plugin_command(
            unk.operation.operation_id,
            unk.operation.revision,
            Some(pa_en.generation),
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .record_plugin_command_interruption(
            claim.operation_id(),
            claim.revision(),
            claim.generation(),
            claim.attempt(),
        )
        .unwrap();

    let settings = state.plugins().get_plugin_settings(&state).await.unwrap();
    let (dk, dr) = req_ctx(
        "POST",
        "/settings/plugins/developer/disable",
        "dev-dis-cas",
        33,
    );
    let err = state
        .plugins()
        .disable_developer_mode(
            &state,
            actor,
            DisableDeveloperModeInput {
                expected_revision: settings.revision,
                idempotency_key: dk,
                request: dr,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    // Even though cas-a failed drain, cas-b MUST still have been drained to Disabled!
    let pb_after = state
        .plugins()
        .get_plugin(&state, "org.example.cas-b")
        .await
        .unwrap();
    assert_eq!(pb_after.lifecycle, PluginLifecycle::Disabled);
}

#[tokio::test]
async fn failed_uninstall_settles_failed_and_clears_active_op() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.unfail", "1.0.0", 1, false);
    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let installed = upload_and_install(&state, pkg, None, grants).await;
    enable_plugin_ok(
        &state,
        "org.example.unfail",
        installed.revision,
        "en-unfail",
    )
    .await;

    // Plant an Unknown runtime command op while enabled, then disable via cancel-skip?
    // Even simpler: insert a Queued management op on the disabled plugin or uninstall after disable with an unresolved op
    let p_en = state
        .store()
        .get_plugin("org.example.unfail")
        .unwrap()
        .unwrap();
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
    let (ck, cr) = req_ctx("POST", "/plugins/org.example.unfail/commands", "unk-un", 40);
    let unk = state
        .store()
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some("org.example.unfail".into()),
                subject: PluginSubject::User(actor),
                idempotency_key: ck,
                request: cr,
                kind: "schedule.set".into(),
                expected_revision: Some(p_en.revision),
                expected_generation: Some(p_en.generation),
                cause: PluginCommandCause::root("unk-un"),
            },
            b"{}",
        )
        .unwrap();
    let claim = state
        .store()
        .claim_plugin_command(
            unk.operation.operation_id,
            unk.operation.revision,
            Some(p_en.generation),
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .record_plugin_command_interruption(
            claim.operation_id(),
            claim.revision(),
            claim.generation(),
            claim.attempt(),
        )
        .unwrap();

    let (uk, ur) = req_ctx(
        "POST",
        "/plugins/org.example.unfail/uninstall",
        "unfail-1",
        41,
    );
    let err = state
        .plugins()
        .uninstall_plugin(
            &state,
            "org.example.unfail",
            UninstallPluginInput {
                actor_user_id: actor,
                remove_private_data: false,
                expected_revision: p_en.revision,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    let ops = state
        .store()
        .list_plugin_operations_admin(Some("org.example.unfail"), None, None, Some(20))
        .unwrap();
    let drain_op = ops.items.iter().find(|o| o.kind == "disable").unwrap();
    assert_eq!(drain_op.state, PluginOperationState::Failed);
    let no_running = ops
        .items
        .iter()
        .all(|o| o.state != PluginOperationState::Running);
    assert!(no_running);
}

#[tokio::test]
async fn disable_plugin_blocked_drain_settles_without_leaving_running() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.disfail", "1.0.0", 1, false);
    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let installed = upload_and_install(&state, pkg, None, grants).await;
    enable_plugin_ok(
        &state,
        "org.example.disfail",
        installed.revision,
        "en-disfail",
    )
    .await;

    let p_en = state
        .plugins()
        .get_plugin(&state, "org.example.disfail")
        .await
        .unwrap();
    let unk_id = plugin_p1c_support::plant_unknown_schedule_cmd(
        &state,
        "org.example.disfail",
        actor,
        "unk-df",
    );

    let (dk, dr) = req_ctx(
        "POST",
        "/plugins/org.example.disfail/disable",
        "dis-fail-1",
        51,
    );
    let err = state
        .plugins()
        .disable_plugin(
            &state,
            "org.example.disfail",
            DisablePluginInput {
                actor_user_id: actor,
                reason: None,
                expected_revision: p_en.revision,
                idempotency_key: dk,
                request: dr,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    let ops = state
        .store()
        .list_plugin_operations_admin(Some("org.example.disfail"), None, None, Some(20))
        .unwrap();
    let dis_op = ops.items.iter().find(|o| o.kind == "disable").unwrap();
    assert_eq!(dis_op.state, PluginOperationState::Failed);

    // Reconcile the unknown op, then reconcile_pending should finalize Draining -> Disabled
    let unk_admin = state
        .store()
        .get_plugin_operation_admin(unk_id)
        .unwrap()
        .unwrap();
    state
        .store()
        .reconcile_plugin_command(&store::plugins::PluginReconciliationObservation {
            operation_id: unk_id,
            revision: unk_admin.operation.revision,
            generation: unk_admin.operation.generation,
            attempt: unk_admin.attempt,
            outcome: store::plugins::PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    reconcile_pending(&state, 100).unwrap();
    let p_final = state
        .plugins()
        .get_plugin(&state, "org.example.disfail")
        .await
        .unwrap();
    assert_eq!(p_final.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(p_final.active_operation_id, None);
}

#[tokio::test]
async fn enable_after_revoke_trust_key_leaves_zero_non_terminal_ops() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();

    let key = TestSigningKey::generate(889);
    let key_view = pin_trust_key(&state, &key).await;
    let pkg = build_task_package("org.example.seqrev", "1.0.0", 1, false);
    let sig = key.sign_package(&pkg);
    let installed = upload_and_install(&state, pkg, Some(sig), vec![]).await;

    let reauth = issue_reauth(&state, ReauthPurpose::TrustKeyRevoke).await;
    let (rk, rr) = req_ctx("POST", "/plugin-trust-keys/revoke", "seq-rev-k", 61);
    state
        .plugins()
        .revoke_trust_key(
            &state,
            actor,
            RevokeTrustKeyInput {
                key_id: key_view.key_id,
                reason: "revoked".into(),
                reauth_token: reauth,
                expected_revision: key_view.revision,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
        .unwrap();

    let (ek, er) = req_ctx("POST", "/plugins/org.example.seqrev/enable", "seq-en", 62);
    let en_err = state
        .plugins()
        .enable_plugin(
            &state,
            "org.example.seqrev",
            EnablePluginInput {
                actor_user_id: actor,
                expected_revision: installed.revision,
                idempotency_key: ek,
                request: er,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(en_err.status_and_code().0, 403);
    let ops = state
        .store()
        .list_plugin_operations_admin(Some("org.example.seqrev"), None, None, Some(20))
        .unwrap();
    assert!(ops.items.iter().all(|o| o.state.is_terminal()));

    let (uk, ur) = req_ctx(
        "POST",
        "/plugins/org.example.seqrev/uninstall",
        "seq-un",
        63,
    );
    let un = state
        .plugins()
        .uninstall_plugin(
            &state,
            "org.example.seqrev",
            UninstallPluginInput {
                actor_user_id: actor,
                remove_private_data: false,
                expected_revision: installed.revision,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap();
    assert_eq!(un.state, PluginOperationState::Succeeded);
}

#[tokio::test]
async fn concurrent_enable_and_revoke_trust_key_leaves_zero_non_terminal_ops() {
    let tmp = tempfile::tempdir().unwrap();
    let state = std::sync::Arc::new(build_state(tmp.path()));

    for iter in 0..4 {
        let k2 = TestSigningKey::generate(900 + iter);
        let kv2 = pin_trust_key(&state, &k2).await;
        let pid = format!("org.example.racerev{iter}");
        let pkg2 = build_task_package(&pid, "1.0.0", 1, false);
        let sig2 = k2.sign_package(&pkg2);
        let inst2 = upload_and_install(&state, pkg2, Some(sig2), vec![]).await;
        let r2 = issue_reauth(&state, ReauthPurpose::TrustKeyRevoke).await;
        let bar = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let (st1, st2, b1, b2) = (state.clone(), state.clone(), bar.clone(), bar.clone());
        let p_clone = pid.clone();
        let kid = kv2.key_id.clone();
        let h_en = tokio::spawn(async move {
            b1.wait().await;
            let (ek, er) = req_ctx(
                "POST",
                &format!("/plugins/{p_clone}/enable"),
                &format!("re-{iter}"),
                64,
            );
            st1.plugins()
                .enable_plugin(
                    &st1,
                    &p_clone,
                    EnablePluginInput {
                        actor_user_id: admin_id(),
                        expected_revision: inst2.revision,
                        idempotency_key: ek,
                        request: er,
                    },
                )
                .await
        });
        let h_rv = tokio::spawn(async move {
            b2.wait().await;
            let (rk, rr) = req_ctx(
                "POST",
                "/plugin-trust-keys/revoke",
                &format!("rr-{iter}"),
                65,
            );
            st2.plugins()
                .revoke_trust_key(
                    &st2,
                    admin_id(),
                    RevokeTrustKeyInput {
                        key_id: kid,
                        reason: "race".into(),
                        reauth_token: r2,
                        expected_revision: kv2.revision,
                        idempotency_key: rk,
                        request: rr,
                    },
                )
                .await
        });
        let _ = h_en.await.unwrap();
        let _ = h_rv.await.unwrap();
        let race_ops = state
            .store()
            .list_plugin_operations_admin(Some(&pid), None, None, Some(20))
            .unwrap();
        assert!(
            race_ops.items.iter().all(|o| o.state.is_terminal()),
            "non-terminal ops leaked in iter {iter}: {:?}",
            race_ops.items
        );
    }
}

#[tokio::test]
async fn permissions_grant_revoke_cascade() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.perms", "1.0.0", 1, false);
    let detail = upload_and_install(&state, pkg, None, vec![]).await;
    let kv_grant = PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let perm_prep = state
        .plugins()
        .prepare_permissions(
            &state,
            actor,
            "org.example.perms",
            PreparePermissionsInput {
                requested_grants: vec![kv_grant.clone()],
            },
        )
        .await
        .unwrap();
    assert_eq!(perm_prep.grant_diff.added.len(), 1);

    let (gk, gr) = req_ctx(
        "POST",
        "/plugins/org.example.perms/permissions/grant",
        "gr-1",
        3,
    );
    let gr_op = state
        .plugins()
        .grant_permissions(
            &state,
            "org.example.perms",
            GrantPermissionsInput {
                actor_user_id: actor,
                consent_token: perm_prep.consent_token,
                grants: vec![kv_grant.clone()],
                expected_revision: detail.revision,
                idempotency_key: gk,
                request: gr,
            },
        )
        .await
        .unwrap();
    assert_eq!(gr_op.state, PluginOperationState::Succeeded);

    let after_grant = state
        .plugins()
        .get_plugin(&state, "org.example.perms")
        .await
        .unwrap();
    enable_plugin_ok(&state, "org.example.perms", after_grant.revision, "en-perm").await;
    let p_en = state
        .store()
        .get_plugin("org.example.perms")
        .unwrap()
        .unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&p_en, None)
        .unwrap();
    state
        .store()
        .plugin_state_put(&auth, PluginStateKind::Kv, "k1", b"hello", 0)
        .unwrap();

    let (rk, rr) = req_ctx(
        "POST",
        "/plugins/org.example.perms/permissions/revoke",
        "rev-1",
        5,
    );
    let rev_op = state
        .plugins()
        .revoke_permissions(
            &state,
            "org.example.perms",
            RevokePermissionsInput {
                actor_user_id: actor,
                grants: vec![kv_grant],
                reason: "least privilege".into(),
                expected_revision: p_en.revision,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
        .unwrap();
    assert_eq!(rev_op.state, PluginOperationState::Succeeded);
    let after_rev = state
        .plugins()
        .get_plugin(&state, "org.example.perms")
        .await
        .unwrap();
    assert_eq!(after_rev.lifecycle, PluginLifecycle::Disabled);
    assert!(after_rev.effective_grants.is_empty());
}
