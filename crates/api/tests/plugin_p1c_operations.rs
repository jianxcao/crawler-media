#[path = "plugin_p1c_support/mod.rs"]
mod plugin_p1c_support;

use std::sync::Arc;

use api::plugins::*;
use plugin_p1c_support::pkg::{TestSigningKey, build_task_package, build_task_package_custom};
use plugin_p1c_support::{
    admin_id, build_state, disable_plugin_ok, enable_dev_mode, enable_plugin_ok,
    exec_state_reset_req, grant_kv_req, grant_permissions_req, issue_reauth, pin_trust_key,
    plant_disabled_guard_op, prepare_permissions_ok, prepare_state_reset_ok,
    reconcile_schedule_cmd, req_ctx, run_task_req, tick, uninstall_plugin_ok, update_task_req,
    upload_and_install,
};
use sha2::{Digest, Sha256};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginGrant,
    PluginGrantScope, PluginOperationState, PluginStateKind, PluginSubject, StagedPluginIntent,
    StagedPluginOpKind,
};
use tokio::sync::Barrier;

#[tokio::test]
async fn state_reset_clears_kv_and_preserves_config_audit() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.sr", "1.0.0", 1, false);
    let kv_grant = PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let detail = upload_and_install(&state, pkg, None, vec![kv_grant]).await;
    enable_plugin_ok(&state, &detail.id, detail.revision, "en-sr").await;

    let p_en = state.store().get_plugin(&detail.id).unwrap().unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&p_en, None)
        .unwrap();
    state
        .store()
        .plugin_state_put(&auth, PluginStateKind::Kv, "k1", b"hello", 0)
        .unwrap();
    let p_dis = disable_plugin_ok(&state, &detail.id, p_en.revision, "dis-sr").await;
    let reset_prep = prepare_state_reset_ok(&state, &detail.id).await;
    assert_eq!(reset_prep.kv_entries, 1);
    let reauth = issue_reauth(&state, ReauthPurpose::PluginStateReset).await;
    let sr_op = exec_state_reset_req(
        &state,
        &detail.id,
        reset_prep.consent_token,
        reauth,
        p_dis.revision,
        "sr-1",
    )
    .await
    .unwrap();
    assert_eq!(sr_op.state, PluginOperationState::Succeeded);
    assert_eq!(
        state
            .store()
            .preview_plugin_state_reset(&detail.id, None)
            .unwrap()
            .kv_entries,
        0
    );
}

async fn drain_ticks(state: &api::ApiState) {
    let tick_state = state.clone();
    tokio::task::spawn_blocking(move || {
        for _ in 0..8 {
            if tick(&tick_state, 50)["ran"].as_u64().unwrap_or(0) == 0 {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn production_effect_adapter_executes_schedule_commands_and_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    assert!(state.plugins().effects().supports_kind("schedule.set"));
    assert!(state.plugins().effects().supports_kind("schedule.disable"));
    assert!(!state.plugins().effects().supports_kind("notify.send"));
    assert!(state.plugins().effects().supports_kind("downloader.add"));

    let grants = vec![
        PluginGrant {
            capability: "command:schedule.set".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "command:notify.send".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
    ];
    let pkg = build_task_package_custom(
        "org.example.sched",
        "1.0.0",
        1,
        false,
        None,
        false,
        &["kv", "command:schedule.set", "command:notify.send"],
    );
    let detail = upload_and_install(&state, pkg, None, grants).await;
    enable_plugin_ok(&state, "org.example.sched", detail.revision, "en-sched").await;
    let enabled = state
        .plugins()
        .get_plugin(&state, "org.example.sched")
        .await
        .unwrap();

    let (bad_k, bad_r) = req_ctx("POST", "/plugins/org.example.sched/commands", "cmd-bad", 11);
    let unsup = state
        .plugins()
        .submit_admin_command(
            &state,
            "org.example.sched",
            SubmitAdminCommandInput {
                actor_user_id: actor,
                kind: "notify.send".into(),
                payload: serde_json::json!({}),
                targets: vec![],
                evidence_ref: None,
                expected_revision: enabled.revision,
                idempotency_key: bad_k,
                request: bad_r,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(unsup.status_and_code().0, 422);

    let (sk, sr) = req_ctx("POST", "/plugins/org.example.sched/commands", "cmd-set", 12);
    let cmd_op = state
        .plugins()
        .submit_admin_command(
            &state,
            "org.example.sched",
            SubmitAdminCommandInput {
                actor_user_id: actor,
                kind: "schedule.set".into(),
                payload: serde_json::json!({
                    "task_id": "sync",
                    "interval_secs": 120,
                    "enabled": true,
                    "expected_revision": 1
                }),
                targets: vec![],
                evidence_ref: Some("admin-sched".into()),
                expected_revision: enabled.revision,
                idempotency_key: sk,
                request: sr,
            },
        )
        .await
        .unwrap();
    assert_eq!(cmd_op.state, PluginOperationState::Queued);
    drain_ticks(&state).await;
    let settled = state
        .plugins()
        .get_operation(&state, cmd_op.operation_id)
        .await
        .unwrap();
    assert_eq!(settled.state, PluginOperationState::Succeeded);
    let binding = state
        .store()
        .get_plugin_task_binding("org.example.sched", "sync")
        .unwrap()
        .unwrap();
    assert!(binding.definition.enabled);
    assert_eq!(binding.definition.interval_secs, Some(120));
    assert_plugin_admin_queries(&state, "org.example.sched").await;
}

async fn assert_plugin_admin_queries(state: &api::ApiState, plugin_id: &str) {
    assert!(
        !state
            .plugins()
            .list_audit(state, plugin_id, None, None, None, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        !state
            .plugins()
            .get_metrics(state, plugin_id, Some(60))
            .await
            .unwrap()
            .draining
    );
    assert!(
        state
            .plugins()
            .list_plugins(state, None, None, None)
            .await
            .unwrap()
            .items
            .iter()
            .any(|p| p.id == plugin_id)
    );
}

#[tokio::test]
async fn submit_admin_command_replay_preserves_http_envelope_body_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let grants = vec![PluginGrant {
        capability: "command:schedule.set".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let pkg = build_task_package("org.example.cmdidem", "1.0.0", 1, false);
    let detail = upload_and_install(&state, pkg, None, grants).await;
    enable_plugin_ok(&state, &detail.id, detail.revision, "en-cmdidem").await;
    let enabled = state
        .plugins()
        .get_plugin(&state, &detail.id)
        .await
        .unwrap();

    // Digest of full HTTP envelope JSON differs from Sha256(input.payload)
    let envelope_digest: [u8; 32] =
        Sha256::digest(br#"{"kind":"schedule.set","payload":{"task_id":"sync"}}"#).into();
    let input = SubmitAdminCommandInput {
        actor_user_id: actor,
        kind: "schedule.set".into(),
        payload: serde_json::json!({
            "task_id": "sync",
            "interval_secs": 180,
            "enabled": true,
            "expected_revision": 1
        }),
        targets: vec![],
        evidence_ref: None,
        expected_revision: enabled.revision,
        idempotency_key: "cmd-env-1".into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugins/org.example.cmdidem/commands".into(),
            body_digest: envelope_digest,
        },
    };
    let op1 = state
        .plugins()
        .submit_admin_command(&state, &detail.id, input.clone())
        .await
        .unwrap();
    let op2 = state
        .plugins()
        .submit_admin_command(&state, &detail.id, input)
        .await
        .unwrap();
    assert_eq!(op1.operation_id, op2.operation_id);
}

#[tokio::test]
async fn task_update_run_receipts_and_trust_key_list_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    let key = TestSigningKey::generate(700);
    let created = pin_trust_key(&state, &key).await;
    let keys = state
        .plugins()
        .list_trust_keys(&state, None, Some(20))
        .await
        .unwrap();
    assert!(keys.items.iter().any(|k| k.key_id == created.key_id));

    let pkg = build_task_package("org.example.tasks", "1.0.0", 1, false);
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
    enable_plugin_ok(&state, "org.example.tasks", detail.revision, "en-tasks").await;
    let tasks = state
        .plugins()
        .list_tasks(&state, "org.example.tasks", None, Some(20))
        .await
        .unwrap();
    assert_eq!(tasks.items.len(), 1);
    let rev = tasks.items[0].definition_revision;

    let (uk, ur) = req_ctx(
        "PUT",
        "/plugins/org.example.tasks/tasks/sync",
        "upd-task-1",
        52,
    );
    let upd_op = state
        .plugins()
        .update_task(
            &state,
            "org.example.tasks",
            "sync",
            UpdateTaskInput {
                actor_user_id: actor,
                enabled: true,
                interval_secs: 180,
                config: serde_json::json!({"action": "ok"}),
                expected_revision: rev,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap();
    assert_eq!(upd_op.state, PluginOperationState::Succeeded);

    let run_op = run_task_req(
        &state,
        "org.example.tasks",
        serde_json::json!({}),
        rev + 1,
        "run-task-1",
    )
    .await
    .unwrap();
    assert_eq!(run_op.state, PluginOperationState::Succeeded);
    assert!(run_op.job_id.is_some());

    let receipts = state
        .plugins()
        .list_task_receipts(&state, "org.example.tasks", "sync", None, Some(20))
        .await
        .unwrap();
    assert!(receipts.items.is_empty());

    let p_now = state
        .plugins()
        .get_plugin(&state, "org.example.tasks")
        .await
        .unwrap();
    let (dk, dr) = req_ctx(
        "POST",
        "/plugins/org.example.tasks/disable",
        "dis-tasks",
        54,
    );
    let dis_op = state
        .plugins()
        .disable_plugin(
            &state,
            "org.example.tasks",
            DisablePluginInput {
                actor_user_id: actor,
                reason: Some("test-disable".into()),
                expected_revision: p_now.revision,
                idempotency_key: dk,
                request: dr,
            },
        )
        .await
        .unwrap();
    assert_eq!(dis_op.state, PluginOperationState::Succeeded);
}

#[tokio::test]
async fn run_task_cas_disabled_guard_input_override_job_id_and_list_tasks_next_run() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.rtpol", "1.0.0", 1, false);
    let grants = vec![PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let detail = upload_and_install(&state, pkg, None, grants).await;
    enable_plugin_ok(&state, &detail.id, detail.revision, "en-rtpol").await;
    let p_en = state
        .plugins()
        .get_plugin(&state, &detail.id)
        .await
        .unwrap();

    let tasks0 = state
        .plugins()
        .list_tasks(&state, &detail.id, None, None)
        .await
        .unwrap();
    assert_eq!(tasks0.items[0].next_run_after, None);
    let dis_err = run_task_req(
        &state,
        &detail.id,
        serde_json::json!({}),
        tasks0.items[0].definition_revision,
        "rt-dis",
    )
    .await
    .unwrap_err();
    assert_eq!(dis_err.status_and_code().0, 409);

    let (uk, ur) = req_ctx("PUT", "/plugins/org.example.rtpol/tasks/sync", "rt-upd", 61);
    state
        .plugins()
        .update_task(
            &state,
            &detail.id,
            "sync",
            UpdateTaskInput {
                actor_user_id: actor,
                enabled: true,
                interval_secs: 300,
                config: serde_json::json!({"action": "submit-and-state"}),
                expected_revision: tasks0.items[0].definition_revision,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap();
    let tasks1 = state
        .plugins()
        .list_tasks(&state, &detail.id, None, None)
        .await
        .unwrap();
    assert!(tasks1.items[0].next_run_after.is_some());
    let def_rev = tasks1.items[0].definition_revision;
    assert_ne!(def_rev, p_en.revision);

    let cas_err = run_task_req(
        &state,
        &detail.id,
        serde_json::json!({}),
        p_en.revision,
        "rt-cas",
    )
    .await
    .unwrap_err();
    assert_eq!(cas_err.status_and_code(), (409, "plugin.revision_conflict"));

    let run_op = run_task_req(
        &state,
        &detail.id,
        serde_json::json!({"action": "block-kv"}),
        def_rev,
        "rt-ok",
    )
    .await
    .unwrap();
    let jid = run_op.job_id.clone().expect("run_task must return job_id");
    let job = state
        .jobs()
        .acquire()
        .unwrap()
        .get(jid.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job.run_after, 0);
    assert!(job.payload.contains("block-kv"));
}

#[tokio::test]
async fn concurrent_duplicate_accepts_replay_cleanly_and_cancel_resume_idempotency() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(build_state(tmp.path()));
    let actor = admin_id();
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.conc", "1.0.0", 1, false);
    let detail = upload_and_install(&state, pkg, None, vec![]).await;

    // Concurrent enable_plugin with identical idempotency_key -> all succeed with same operation_id
    let barrier = Arc::new(Barrier::new(4));
    let mut futs = Vec::new();
    for _ in 0..4 {
        let (st, bar) = (state.clone(), barrier.clone());
        let rev = detail.revision;
        futs.push(tokio::spawn(async move {
            bar.wait().await;
            let (ek, er) = req_ctx("POST", "/plugins/org.example.conc/enable", "conc-en", 70);
            st.plugins()
                .enable_plugin(
                    &st,
                    "org.example.conc",
                    EnablePluginInput {
                        actor_user_id: admin_id(),
                        expected_revision: rev,
                        idempotency_key: ek,
                        request: er,
                    },
                )
                .await
        }));
    }
    let mut op_ids = Vec::new();
    for h in futs {
        let res = h.await.unwrap().unwrap();
        assert!(matches!(
            res.state,
            PluginOperationState::Queued
                | PluginOperationState::Running
                | PluginOperationState::Succeeded
        ));
        op_ids.push(res.operation_id);
    }
    assert!(op_ids.windows(2).all(|w| w[0] == w[1]));
    let settled = state
        .plugins()
        .get_operation(&state, op_ids[0])
        .await
        .unwrap();
    assert_eq!(settled.state, PluginOperationState::Succeeded);

    // Cancel + Resume idempotency on a queued staged operation
    let (ik, ir) = req_ctx("POST", "/plugin-installations", "conc-staged", 71);
    let accepted = state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: ik,
            request: ir,
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: "org.example.conc2".into(),
                package_hash: Some("deadbeef".into()),
                version: Some("1.0.0".into()),
                artifact_path: None,
                state_policy: None,
                target_config_digest: None,
                grants: vec![],
            },
            cause: PluginCommandCause::root("conc-staged"),
        })
        .unwrap();
    let (ck, cr) = req_ctx("POST", "/plugin-operations/cancel", "conc-can", 72);
    let can_in = CancelOperationInput {
        actor_user_id: actor,
        reason: Some("test".into()),
        expected_revision: accepted.operation.revision,
        idempotency_key: ck,
        request: cr,
    };
    let c1 = state
        .plugins()
        .cancel_operation(&state, accepted.operation.operation_id, can_in.clone())
        .await
        .unwrap();
    let c2 = state
        .plugins()
        .cancel_operation(&state, accepted.operation.operation_id, can_in)
        .await
        .unwrap();
    assert_eq!(c1, c2);
}

#[tokio::test]
async fn state_reset_and_grant_not_drained_preserve_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.tokpres", "1.0.0", 1, false);
    let detail = upload_and_install(&state, pkg, None, vec![]).await;
    enable_plugin_ok(&state, &detail.id, detail.revision, "en-tp").await;
    let p_en = state
        .plugins()
        .get_plugin(&state, &detail.id)
        .await
        .unwrap();

    let sr_prep = prepare_state_reset_ok(&state, &detail.id).await;
    let sr_reauth = issue_reauth(&state, ReauthPurpose::PluginStateReset).await;
    let sr_err = exec_state_reset_req(
        &state,
        &detail.id,
        sr_prep.consent_token,
        sr_reauth.clone(),
        p_en.revision,
        "tp-sr-1",
    )
    .await
    .unwrap_err();
    assert_eq!(sr_err.status_and_code(), (409, "plugin.not_drained"));

    let kv_grant = PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let gp_prep = prepare_permissions_ok(&state, &detail.id, vec![kv_grant.clone()]).await;
    let gp_err = grant_permissions_req(
        &state,
        &detail.id,
        gp_prep.consent_token.clone(),
        vec![kv_grant.clone()],
        p_en.revision,
        "tp-gp-1",
    )
    .await
    .unwrap_err();
    assert_eq!(gp_err.status_and_code(), (409, "plugin.not_drained"));

    let p_dis = disable_plugin_ok(&state, &detail.id, p_en.revision, "tp-dis").await;
    let gp_old = grant_permissions_req(
        &state,
        &detail.id,
        gp_prep.consent_token,
        vec![kv_grant],
        p_en.revision,
        "tp-gp-2",
    )
    .await
    .unwrap_err();
    assert_eq!(gp_old.status_and_code(), (409, "plugin.revision_conflict"));

    let sr_prep2 = prepare_state_reset_ok(&state, &detail.id).await;
    let sr_ok = exec_state_reset_req(
        &state,
        &detail.id,
        sr_prep2.consent_token,
        sr_reauth,
        p_dis.revision,
        "tp-sr-ok",
    )
    .await
    .unwrap();
    assert_eq!(sr_ok.state, PluginOperationState::Succeeded);
}

#[tokio::test]
async fn state_reset_unresolved_cmd_preserves_consent_and_missing_schema_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.srcons", "1.0.0", 1, false);
    let p_dis = upload_and_install(&state, pkg, None, vec![]).await;
    let unk = plant_disabled_guard_op(&state, &p_dis.id, p_dis.revision, "src-dis-unk");
    let sr_prep = prepare_state_reset_ok(&state, &p_dis.id).await;
    let sr_reauth = issue_reauth(&state, ReauthPurpose::PluginStateReset).await;
    let err = exec_state_reset_req(
        &state,
        &p_dis.id,
        sr_prep.consent_token.clone(),
        sr_reauth.clone(),
        p_dis.revision,
        "src-sr-1",
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_and_code(), (409, "plugin.not_drained"));

    reconcile_schedule_cmd(&state, unk);
    let ok = exec_state_reset_req(
        &state,
        &p_dis.id,
        sr_prep.consent_token,
        sr_reauth,
        p_dis.revision,
        "src-sr-2",
    )
    .await
    .unwrap();
    assert_eq!(ok.state, PluginOperationState::Succeeded);

    let p_after = state.plugins().get_plugin(&state, &p_dis.id).await.unwrap();
    enable_plugin_ok(&state, &p_dis.id, p_after.revision, "en-src-2").await;
    let p_en2 = state.plugins().get_plugin(&state, &p_dis.id).await.unwrap();
    let ver = state
        .store()
        .get_plugin_version(&p_dis.id, &p_en2.package_hash)
        .unwrap()
        .unwrap();
    std::fs::remove_file(
        tmp.path()
            .join("data")
            .join(&ver.artifact_path)
            .join("schemas/task.schema.json"),
    )
    .unwrap();
    let rt_err = run_task_req(
        &state,
        &p_dis.id,
        serde_json::json!({"bad_field": 1}),
        p_en2.revision,
        "src-rt-fail",
    )
    .await
    .unwrap_err();
    assert_eq!(rt_err.status_and_code().0, 400);
}

#[tokio::test]
async fn failed_task_and_grant_ops_leave_zero_unresolved_and_allow_uninstall() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(tmp.path());
    enable_dev_mode(&state).await;

    let pkg = build_task_package("org.example.atomops", "1.0.0", 1, false);
    let detail = upload_and_install(&state, pkg, None, vec![]).await;
    enable_plugin_ok(&state, &detail.id, detail.revision, "en-atom").await;
    let p_en = state
        .plugins()
        .get_plugin(&state, &detail.id)
        .await
        .unwrap();

    assert_eq!(
        run_task_req(&state, &detail.id, serde_json::json!({}), 1, "atom-rt-dis")
            .await
            .unwrap_err()
            .status_and_code()
            .0,
        409
    );
    assert_eq!(
        run_task_req(
            &state,
            &detail.id,
            serde_json::json!({"bad": 1}),
            1,
            "atom-rt-sch"
        )
        .await
        .unwrap_err()
        .status_and_code(),
        (400, "plugin.config_invalid")
    );
    assert_eq!(
        update_task_req(
            &state,
            &detail.id,
            serde_json::json!({"action": "ok"}),
            999,
            "atom-ut-1"
        )
        .await
        .unwrap_err()
        .status_and_code(),
        (409, "plugin.revision_conflict")
    );
    assert_eq!(
        update_task_req(
            &state,
            &detail.id,
            serde_json::json!({"unknown_key": true}),
            1,
            "atom-ut-2"
        )
        .await
        .unwrap_err()
        .status_and_code(),
        (400, "plugin.config_invalid")
    );
    assert_eq!(
        grant_kv_req(&state, &detail.id, p_en.revision, "atom-gr")
            .await
            .unwrap_err()
            .status_and_code(),
        (409, "plugin.not_drained")
    );

    let ops = state
        .store()
        .list_plugin_operations_admin(Some(&detail.id), None, None, Some(50))
        .unwrap();
    assert!(ops.items.iter().all(|o| o.state.is_terminal()));
    uninstall_plugin_ok(&state, &detail.id, p_en.revision, "atom-un").await;
}
