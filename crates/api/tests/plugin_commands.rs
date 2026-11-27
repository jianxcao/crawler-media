#[path = "plugin_commands/support.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use api::plugins::{
    PluginInvocationRole, PluginServiceError, SubmitPluginCommand, reconcile_pending,
    recover_startup, submit_command,
};
use domain::PluginOperationId;
use jobs::Runner;
use store::plugins::*;
use store::{ManualCredentialWrite, StoreError};
use support::*;

#[test]
fn idempotency_conflict_and_subject_scope_isolation() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Succeed));
    let state = build_state(tmp.path(), effects.clone(), None);
    let p = install_and_enable(state.store().as_ref(), "org.example.idem", &[]);
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    let subject = PluginSubject::EndpointKey {
        key_id: uuid::Uuid::new_v4(),
        issuer_user_id: domain::UserId::new(),
    };
    let req = SubmitPluginCommand {
        role: PluginInvocationRole::Endpoint,
        authority: auth.clone(),
        subject: subject.clone(),
        kind: "fixture".into(),
        targets: vec![PluginCommandTarget::Global],
        idempotency_key: "key-1".into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugins/fixture/run".into(),
            body_digest: [5; 32],
        },
        expected_revision: None,
        cause: PluginCommandCause::root("trace-1"),
        payload: b"secret-input".to_vec(),
    };
    let first = submit_command(&state, req.clone()).unwrap();
    assert!(!first.replayed);
    let mut replay_req = req.clone();
    replay_req.expected_revision = Some(999);
    let second = submit_command(&state, replay_req).unwrap();
    assert!(second.replayed);
    assert_eq!(first.operation.operation_id, second.operation.operation_id);

    let mut changed = req.clone();
    changed.request.body_digest = [9; 32];
    assert!(matches!(
        submit_command(&state, changed),
        Err(PluginServiceError::Store(StoreError::Plugin(
            PluginError::Conflict
        )))
    ));
    // Caveat #3: ungranted command returns Denied BEFORE supports_kind.
    let mut ungranted = req.clone();
    ungranted.kind = "unwired.production".into();
    ungranted.idempotency_key = "key-2-denied".into();
    assert!(matches!(
        submit_command(&state, ungranted),
        Err(PluginServiceError::Store(StoreError::Plugin(
            PluginError::Denied
        )))
    ));

    let op_id = PluginOperationId::from_uuid(first.operation.operation_id);
    let jobs_writer = state.jobs();
    assert!(
        jobs_writer
            .acquire()
            .unwrap()
            .plugin_operation_receipt(op_id, jobs::JobKind::PluginCommand, 1)
            .unwrap()
            .is_some()
    );
    tick(&state, 10);
    assert_eq!(effects.calls(), 1);
    let stranger = PluginSubject::User(domain::UserId::new());
    assert!(
        state
            .store()
            .get_plugin_operation(&stranger, op_id.as_uuid())
            .unwrap()
            .is_none()
    );
    let res = state
        .store()
        .plugin_operation_result(&auth, &subject, op_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(res.as_bytes(), format!("receipt:{op_id}").as_bytes());
}

#[test]
fn granted_unsupported_command_returns_unsupported_command() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Succeed));
    let state = build_state(tmp.path(), effects, None);
    let meta_unsup = PluginVersionMetadata {
        plugin_id: "org.example.unsup".into(),
        package_hash: "pkg-unsup-1".into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "/unused/pkg".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    };
    let inst_u = state
        .store()
        .register_plugin_version(&meta_unsup, 0)
        .unwrap();
    let granted_u = state
        .store()
        .replace_plugin_grants(
            "org.example.unsup",
            &inst_u.package_hash,
            inst_u.revision,
            &[PluginGrant {
                capability: "command:unwired.production".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }],
        )
        .unwrap();
    let enabled_u = enable(state.store().as_ref(), &granted_u);
    let auth_u = state
        .store()
        .issue_plugin_state_authority(&enabled_u, None)
        .unwrap();
    let req = SubmitPluginCommand {
        role: PluginInvocationRole::Endpoint,
        authority: auth_u,
        subject: PluginSubject::User(domain::UserId::new()),
        kind: "unwired.production".into(),
        targets: vec![PluginCommandTarget::Global],
        idempotency_key: "key-2".into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugins/fixture/run".into(),
            body_digest: [5; 32],
        },
        expected_revision: None,
        cause: PluginCommandCause::root("trace-1"),
        payload: b"secret-input".to_vec(),
    };
    assert!(matches!(
        submit_command(&state, req),
        Err(PluginServiceError::UnsupportedCommand(_))
    ));
}

#[test]
fn crash_recovery_projects_pending_intents_once_and_marks_running_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let (queued_id, running_id, binding_id) = seed_pre_crash_state(tmp.path());

    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Succeed));
    let state = build_state(tmp.path(), effects.clone(), None);
    recover_startup(&state).unwrap();
    recover_startup(&state).unwrap();

    let running_after = state
        .store()
        .plugin_command_status(running_id)
        .unwrap()
        .unwrap()
        .operation;
    assert_eq!(running_after.state, PluginOperationState::Unknown);
    assert_eq!(running_after.phase, "worker-interrupted");
    assert!(
        !state
            .store()
            .get_plugin_task_binding_by_id(binding_id)
            .unwrap()
            .unwrap()
            .projection_pending()
    );
    assert!(task_fence(&state, binding_id).is_some());

    tick(&state, 20);
    tick(&state, 21);
    assert_eq!(effects.calls(), 1);
    assert_eq!(
        *effects.executed.lock(),
        vec![PluginOperationId::from_uuid(queued_id)]
    );
}

#[test]
fn revocation_disable_and_epoch_rotation_block_effect_and_stale_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Succeed));
    let state = build_state(tmp.path(), effects.clone(), None);
    let s = site();
    state.store().insert_site(&s).unwrap();
    let p = install_and_enable(
        state.store().as_ref(),
        "org.example.stale",
        std::slice::from_ref(&s),
    );
    let subject = PluginSubject::User(domain::UserId::new());
    let obs = state.store().observe_site(s.id).unwrap().unwrap();
    let auth1 = state
        .store()
        .issue_plugin_state_authority(&p, Some(&obs))
        .unwrap();

    let cmd_pre = submit(
        &state,
        PluginInvocationRole::Endpoint,
        auth1,
        subject.clone(),
        "pre-claim-stale",
        PluginCommandCause::root("c1"),
        Some(s.id),
    )
    .unwrap()
    .operation;
    state
        .store()
        .save_site(&s, ManualCredentialWrite::Replace)
        .unwrap();
    tick(&state, 10);
    assert_eq!(effects.calls(), 0);
    assert_eq!(
        op_state(&state, &subject, cmd_pre.operation_id).state,
        PluginOperationState::Rejected
    );

    let obs2 = state.store().observe_site(s.id).unwrap().unwrap();
    let auth2 = state
        .store()
        .issue_plugin_state_authority(&p, Some(&obs2))
        .unwrap();
    submit(
        &state,
        PluginInvocationRole::Endpoint,
        auth2,
        subject.clone(),
        "gen-stale",
        PluginCommandCause::root("c2"),
        Some(s.id),
    )
    .unwrap();
    let disabled = disable(state.store().as_ref(), &p);
    let enabled = enable(state.store().as_ref(), &disabled);
    tick(&state, 11);
    assert_eq!(effects.calls(), 0);

    let hook_store = state.store();
    let hook_site = s.clone();
    let state = state.with_plugin_pre_effect_hook(Arc::new(move |_| {
        hook_store
            .save_site(&hook_site, ManualCredentialWrite::Replace)
            .unwrap();
    }));
    let obs3 = state.store().observe_site(s.id).unwrap().unwrap();
    let auth3 = state
        .store()
        .issue_plugin_state_authority(&enabled, Some(&obs3))
        .unwrap();
    let cmd_race = submit(
        &state,
        PluginInvocationRole::Endpoint,
        auth3,
        subject.clone(),
        "in-flight-race",
        PluginCommandCause::root("c3"),
        Some(s.id),
    )
    .unwrap()
    .operation;
    tick(&state, 12);
    assert_eq!(effects.calls(), 0);
    assert_eq!(
        op_state(&state, &subject, cmd_race.operation_id).state,
        PluginOperationState::Rejected
    );
}

#[test]
fn unknown_effect_persists_across_restart_and_settles_only_via_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::UnknownThenReconcile));
    let state = build_state(tmp.path(), effects.clone(), None);
    let p = install_and_enable(state.store().as_ref(), "org.example.unknown", &[]);
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let op = submit(
        &state,
        PluginInvocationRole::Endpoint,
        auth,
        subject.clone(),
        "lost-reply",
        PluginCommandCause::root("c-unk"),
        None,
    )
    .unwrap()
    .operation;
    tick(&state, 10);
    assert_eq!(effects.calls(), 1);
    assert_eq!(
        op_state(&state, &subject, op.operation_id).state,
        PluginOperationState::Unknown
    );
    drop(state);

    let effects2 = Arc::new(FakeEffects::new(FakeEffectMode::UnknownThenReconcile));
    *effects2.receipts.lock() = effects.receipts.lock().clone();
    let reopened = build_state(tmp.path(), effects2.clone(), None);
    recover_startup(&reopened).unwrap();
    tick(&reopened, 20);
    assert_eq!(effects2.calls(), 0);
    let cur = op_state(&reopened, &subject, op.operation_id);
    assert_eq!(cur.state, PluginOperationState::Unknown);
    assert!(matches!(
        reopened
            .store()
            .cancel_plugin_operation(&subject, cur.operation_id, cur.revision),
        Err(StoreError::Plugin(PluginError::UnknownEffect))
    ));
    assert!(matches!(
        reopened
            .store()
            .resume_plugin_command(&subject, cur.operation_id, cur.revision),
        Err(StoreError::Plugin(PluginError::UnknownEffect))
    ));

    *effects2.reconcile_outcome.lock() = Some(PluginReconciliationOutcome::Succeeded);
    tick(&reopened, 200);
    assert_eq!(effects2.calls(), 0);
    let settled = op_state(&reopened, &subject, op.operation_id);
    assert_eq!(settled.state, PluginOperationState::Succeeded);
    assert_eq!(settled.effect, PluginEffectObservation::Confirmed);
}

fn submit_task_hop(
    state: &api::ApiState,
    pid: &str,
    idem: &str,
    cause: PluginCommandCause,
) -> Result<PluginOperation, PluginServiceError> {
    let p = install_and_enable(state.store().as_ref(), pid, &[]);
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    let subj = PluginSubject::Task {
        plugin_id: pid.into(),
        task_id: "step".into(),
    };
    Ok(submit(
        state,
        PluginInvocationRole::Task,
        auth,
        subj,
        idem,
        cause,
        None,
    )?
    .operation)
}

#[test]
fn cause_loop_depth_limit_and_wrong_context_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Succeed));
    let state = build_state(tmp.path(), effects, None);
    let p_a = install_and_enable(state.store().as_ref(), "org.example.loop_a", &[]);
    let auth_a = state
        .store()
        .issue_plugin_state_authority(&p_a, None)
        .unwrap();
    let user = PluginSubject::User(domain::UserId::new());
    for role in [
        PluginInvocationRole::Filter,
        PluginInvocationRole::Gate,
        PluginInvocationRole::Notify,
    ] {
        let err = submit(
            &state,
            role,
            auth_a.clone(),
            user.clone(),
            "bad-role",
            PluginCommandCause::root("c"),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, PluginServiceError::WrongContext));
    }

    let mut cause = PluginCommandCause::root("c-depth");
    for idx in 0..4 {
        let pid = format!("org.example.hop_{idx}");
        let op = submit_task_hop(&state, &pid, &format!("hop-{idx}"), cause.clone()).unwrap();
        let claim = state
            .store()
            .claim_plugin_command(op.operation_id, op.revision, op.generation, 0, 10)
            .unwrap();
        cause = state
            .store()
            .inspect_claimed_plugin_command(&claim)
            .unwrap()
            .cause;
        assert_eq!(cause.depth as usize, idx + 1);
    }

    assert!(matches!(
        submit_task_hop(&state, "org.example.hop_4", "hop-4", cause.clone()),
        Err(PluginServiceError::Store(StoreError::Plugin(
            PluginError::CauseLoop
        )))
    ));

    let hop1 = state
        .store()
        .get_plugin("org.example.hop_1")
        .unwrap()
        .unwrap();
    let auth1 = state
        .store()
        .issue_plugin_state_authority(&hop1, None)
        .unwrap();
    let subj1 = PluginSubject::Task {
        plugin_id: "org.example.hop_1".into(),
        task_id: "step".into(),
    };
    let cycle = PluginCommandCause {
        cause_id: cause.cause_id,
        depth: 2,
        visited: cause.visited[..2].to_vec(),
    };
    assert!(matches!(
        submit(
            &state,
            PluginInvocationRole::Task,
            auth1,
            subj1,
            "cycle",
            cycle,
            None
        ),
        Err(PluginServiceError::Store(StoreError::Plugin(
            PluginError::CauseLoop
        )))
    ));
}

#[test]
fn distinct_task_bindings_dispatch_independently_and_close_on_disable() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Succeed));
    let tasks = Arc::new(FakeTaskRunner::default());
    let state = build_state(tmp.path(), effects, Some(tasks.clone()));
    let p = install_and_enable(state.store().as_ref(), "org.example.tasks", &[]);
    let save = |name: &str, secs: i64, tag: &str| {
        state
            .store()
            .save_plugin_task_binding(
                &p,
                name,
                0,
                &PluginTaskDefinition {
                    enabled: true,
                    interval_secs: Some(secs),
                    public_config: serde_json::json!({"tag": tag}),
                },
            )
            .unwrap()
    };
    let (b1, b2) = (save("sync-a", 600, "a"), save("sync-b", 900, "b"));
    reconcile_pending(&state, 1).unwrap();
    let (f1, f2) = (
        task_fence(&state, b1.binding_id).unwrap(),
        task_fence(&state, b2.binding_id).unwrap(),
    );
    assert_ne!(f1.def_id, f2.def_id);

    tick(&state, 10);
    tick(&state, 11);
    let mut seen = tasks.seen.lock().clone();
    seen.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        seen,
        vec![
            ("sync-a".into(), serde_json::json!({"tag":"a"})),
            ("sync-b".into(), serde_json::json!({"tag":"b"}))
        ]
    );

    let jobs_writer = state.jobs();
    let stale_job = jobs_writer
        .acquire()
        .unwrap()
        .jobs_for_identity(
            jobs::JobIdentity::plugin_task(domain::PluginTaskBindingId::from_uuid(b1.binding_id)),
            1,
        )
        .unwrap()
        .remove(0);
    let _ = disable(state.store().as_ref(), &p);
    reconcile_pending(&state, 20).unwrap();
    assert!(task_fence(&state, b1.binding_id).unwrap().closed);

    api::worker::StateRunner {
        state: state.clone(),
    }
    .run(&stale_job)
    .unwrap();
    assert_eq!(tasks.count.load(Ordering::Relaxed), 2);
}

#[test]
fn blocked_effect_retains_active_attempt_fence_against_timeout_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Blocked(gate.clone())));
    let state = build_state(tmp.path(), effects.clone(), None);
    let p = install_and_enable(state.store().as_ref(), "org.example.blocked", &[]);
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let op = submit(
        &state,
        PluginInvocationRole::Endpoint,
        auth,
        subject.clone(),
        "blocking-op",
        PluginCommandCause::root("c-block"),
        None,
    )
    .unwrap()
    .operation;

    let bg_state = state.clone();
    let worker = std::thread::spawn(move || tick(&bg_state, 10));
    gate.wait_entered();

    tick(&state, 1000);
    assert_eq!(
        op_state(&state, &subject, op.operation_id).state,
        PluginOperationState::Running
    );
    assert_eq!(effects.calls(), 1);

    gate.release();
    worker.join().unwrap();

    assert_eq!(
        op_state(&state, &subject, op.operation_id).state,
        PluginOperationState::Succeeded
    );
    assert_eq!(effects.calls(), 1);
}

#[test]
fn unsanitized_effect_phase_settles_cleanly_without_wedging_running() {
    let tmp = tempfile::tempdir().unwrap();
    let effects = Arc::new(FakeEffects::new(FakeEffectMode::Reject(
        "download.before hook rejected: bad tag! (rule #42)".into(),
    )));
    let state = build_state(tmp.path(), effects.clone(), None);
    let p = install_and_enable(state.store().as_ref(), "org.example.unsanitized", &[]);
    let auth = state
        .store()
        .issue_plugin_state_authority(&p, None)
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let op = submit(
        &state,
        PluginInvocationRole::Endpoint,
        auth,
        subject.clone(),
        "unsanitized-phase",
        PluginCommandCause::root("c-unsan"),
        None,
    )
    .unwrap()
    .operation;

    tick(&state, 10);
    let settled = op_state(&state, &subject, op.operation_id);
    assert_eq!(settled.state, PluginOperationState::Rejected);
    assert_eq!(
        settled.phase,
        "download.before-hook-rejected:-bad-tag-rule-42"
    );
}
