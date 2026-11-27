use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};
use store::{Db, Store, StoreError, plugins::*};

fn staged_accept(
    plugin_id: &str,
    hash: &str,
    version: &str,
    op_kind: StagedPluginOpKind,
    expected_revision: i64,
    key: &str,
) -> AcceptStagedPluginOperation {
    AcceptStagedPluginOperation {
        subject: PluginSubject::User(domain::UserId::new()),
        idempotency_key: key.into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugins/install".into(),
            body_digest: [7; 32],
        },
        op_kind,
        expected_revision,
        intent: StagedPluginIntent {
            target_plugin_id: plugin_id.into(),
            package_hash: Some(hash.into()),
            version: Some(version.into()),
            artifact_path: None,
            state_policy: Some("preserve".into()),
            target_config_digest: None,
            grants: vec![PluginGrant {
                capability: "kv".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }],
        },
        cause: PluginCommandCause::root(format!("cause-{key}")),
    }
}

fn version_meta(plugin_id: &str, hash: &str, version: &str) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: plugin_id.into(),
        package_hash: hash.into(),
        version: version.into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: format!("/plugins/{plugin_id}/{hash}.cmplugin"),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

fn interrupt_and_recover(
    store: &Store,
    op_id: uuid::Uuid,
    revision: i64,
    generation: Option<i64>,
    attempt: i64,
    resume_phase: &str,
    now: i64,
) -> PluginCommandClaim {
    let interrupted = store
        .record_plugin_command_interruption(op_id, revision, generation, attempt)
        .unwrap();
    assert_eq!(interrupted.state, PluginOperationState::Unknown);
    store
        .recover_staged_plugin_operation(
            interrupted.operation_id,
            interrupted.revision,
            attempt,
            resume_phase,
            now,
        )
        .unwrap()
}

#[test]
fn staged_install_persists_intent_before_fs_and_resumes_across_every_crash_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let input = staged_accept(
        "org.example.staged",
        "hash-v1",
        "1.0.0",
        StagedPluginOpKind::Install,
        0,
        "k1",
    );
    let accepted = store.begin_staged_plugin_operation(&input).unwrap();
    assert!(!accepted.replayed);
    assert_eq!(accepted.operation.state, PluginOperationState::Queued);
    assert_eq!(accepted.operation.phase, "accepted");
    assert!(
        store
            .referenced_plugin_package_hashes()
            .unwrap()
            .contains("hash-v1")
    );

    // Crash after claim + stage checkpoint
    let mut claim = store
        .claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            100,
        )
        .unwrap();
    let old_claim = claim.clone();
    let mut staged_intent = accepted.intent.clone();
    staged_intent.artifact_path = Some("/plugins/org.example.staged/hash-v1.cmplugin".into());
    let op = store
        .advance_staged_plugin_phase(&mut claim, "staged", Some(&staged_intent), 101)
        .unwrap();
    assert_eq!(op.state, PluginOperationState::Running);
    assert_eq!(op.phase, "staged");
    assert!(matches!(
        store.complete_staged_plugin_operation(
            &old_claim,
            PluginOperationState::Succeeded,
            "done",
            None,
            None,
            102
        ),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    // Reopen, verify unresolved list, recover from Unknown and register version
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .list_unresolved_staged_plugin_operations(None, None)
            .unwrap()
            .items
            .len(),
        1
    );
    let mut recovered = interrupt_and_recover(
        &reopened,
        op.operation_id,
        op.revision,
        op.generation,
        claim.attempt(),
        "registering",
        200,
    );
    let meta = version_meta("org.example.staged", "hash-v1", "1.0.0");
    let plugin = reopened
        .register_staged_plugin_version(&mut recovered, &meta, &input.intent.grants, 201)
        .unwrap();
    assert_eq!(plugin.lifecycle, PluginLifecycle::InstalledDisabled);
    assert_eq!(
        plugin.active_operation_id,
        Some(accepted.operation.operation_id)
    );

    // Crash again AFTER version registration, then recover and idempotently re-run registration
    let status = reopened
        .get_plugin_operation_admin(accepted.operation.operation_id)
        .unwrap()
        .unwrap();
    let mut recovered2 = interrupt_and_recover(
        &reopened,
        status.operation.operation_id,
        status.operation.revision,
        status.operation.generation,
        status.attempt,
        "registering",
        300,
    );
    let plugin_retry = reopened
        .register_staged_plugin_version(&mut recovered2, &meta, &input.intent.grants, 301)
        .unwrap();
    assert_eq!(plugin_retry.plugin_id, "org.example.staged");

    let finished = reopened
        .complete_staged_plugin_operation(
            &recovered2,
            PluginOperationState::Succeeded,
            "completed",
            Some(&serde_json::json!({"installed":true})),
            None,
            302,
        )
        .unwrap();
    assert_eq!(finished.state, PluginOperationState::Succeeded);
    let final_plugin = reopened.get_plugin("org.example.staged").unwrap().unwrap();
    assert_eq!(final_plugin.active_operation_id, None);
    assert_eq!(final_plugin.lifecycle, PluginLifecycle::InstalledDisabled);
}

#[test]
fn idempotency_key_replays_after_completion_and_rejects_mismatched_payload_with_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let input = staged_accept(
        "org.example.idem",
        "hash-1",
        "1.0.0",
        StagedPluginOpKind::Install,
        0,
        "idem-1",
    );
    let first = store.begin_staged_plugin_operation(&input).unwrap();
    let mut claim = store
        .claim_staged_plugin_operation(
            first.operation.operation_id,
            first.operation.revision,
            0,
            10,
        )
        .unwrap();
    store
        .register_staged_plugin_version(
            &mut claim,
            &version_meta("org.example.idem", "hash-1", "1.0.0"),
            &[],
            11,
        )
        .unwrap();
    store
        .complete_staged_plugin_operation(
            &claim,
            PluginOperationState::Succeeded,
            "completed",
            None,
            None,
            12,
        )
        .unwrap();

    // Replay returns same operation_id even after plugin was created (revision is now 1, input had 0)
    let replay = store.begin_staged_plugin_operation(&input).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.operation.operation_id, first.operation.operation_id);

    let mut wrong = input.clone();
    wrong.request.body_digest = [9; 32];
    assert!(matches!(
        store.begin_staged_plugin_operation(&wrong),
        Err(StoreError::Plugin(PluginError::IdempotencyConflict))
    ));
    assert!(matches!(
        store.get_plugin_operation_by_idempotency(
            &input.subject,
            Some("org.example.idem"),
            "idem-1",
            &wrong.request,
            "install"
        ),
        Err(StoreError::Plugin(PluginError::IdempotencyConflict))
    ));
    assert_eq!(
        store
            .get_plugin_operation_by_idempotency(
                &input.subject,
                Some("org.example.idem"),
                "idem-1",
                &input.request,
                "install"
            )
            .unwrap()
            .unwrap()
            .operation_id,
        first.operation.operation_id
    );
}

#[test]
fn active_and_inflight_operations_block_concurrent_installs_and_cancel_releases_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let op1 = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.lock",
            "h1",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "k1",
        ))
        .unwrap();
    let op2_err = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.lock",
            "h2",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "k2",
        ))
        .unwrap_err();
    assert!(
        matches!(op2_err, StoreError::Plugin(PluginError::OperationActive(id)) if id == op1.operation.operation_id)
    );

    let cancelled = store
        .cancel_plugin_operation_admin(op1.operation.operation_id, op1.operation.revision, 50)
        .unwrap();
    assert_eq!(cancelled.state, PluginOperationState::Cancelled);

    // Lock released; new install succeeds
    let op2 = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.lock",
            "h2",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "k2",
        ))
        .unwrap();
    assert_ne!(op2.operation.operation_id, op1.operation.operation_id);
}

#[test]
fn immutable_plugin_versions_detects_hash_conflict_and_referenced_hashes_union() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let p = store
        .register_plugin_version(&version_meta("org.example.ver", "hash-1", "1.0.0"), 0)
        .unwrap();
    assert!(matches!(
        store.register_plugin_version(
            &version_meta("org.example.ver", "hash-other", "1.0.0"),
            p.revision
        ),
        Err(StoreError::Plugin(PluginError::PackageConflict))
    ));
    store
        .register_plugin_version(
            &version_meta("org.example.ver", "hash-2", "2.0.0"),
            p.revision,
        )
        .unwrap();
    assert_eq!(
        store
            .get_plugin_version_by_semver("org.example.ver", "1.0.0")
            .unwrap()
            .unwrap()
            .package_hash,
        "hash-1"
    );
    let versions = store
        .list_plugin_versions("org.example.ver", None, None)
        .unwrap();
    assert_eq!(versions.items.len(), 2);

    // Pre-registration queued op protects hash-staged; cancelled op does not
    let staged = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.future",
            "hash-staged",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "k-s",
        ))
        .unwrap();
    let cancelled_op = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.dead",
            "hash-dead",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "k-d",
        ))
        .unwrap();
    store
        .cancel_plugin_operation_admin(
            cancelled_op.operation.operation_id,
            cancelled_op.operation.revision,
            99,
        )
        .unwrap();

    let (tx_locked, rx_locked) = mpsc::channel();
    let (tx_release, rx_release) = mpsc::channel();
    let writer_store = store.clone();
    let handle = std::thread::spawn(move || {
        writer_store
            .with_writer(Db::App, |_| {
                tx_locked.send(()).unwrap();
                rx_release.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    rx_locked.recv().unwrap();
    let refs: BTreeSet<String> = store.referenced_plugin_package_hashes().unwrap();
    assert!(refs.contains("hash-1"));
    assert!(refs.contains("hash-2"));
    assert!(refs.contains("hash-staged"));
    assert!(!refs.contains("hash-dead"));
    assert_eq!(
        store
            .get_active_plugin_operation("org.example.future")
            .unwrap()
            .unwrap()
            .operation_id,
        staged.operation.operation_id
    );
    tx_release.send(()).unwrap();
    handle.join().unwrap();
}

fn accept_mgmt_op(
    store: &Store,
    plugin_id: &str,
    revision: i64,
    kind: &str,
    key: &str,
) -> PluginOperation {
    store
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(plugin_id.into()),
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: key.into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{plugin_id}/{kind}"),
                body_digest: [5; 32],
            },
            kind: kind.into(),
            expected_revision: Some(revision),
            expected_generation: None,
            cause: PluginCommandCause::root(format!("cause-{key}")),
        })
        .unwrap()
        .operation
}

#[test]
fn reconcile_staged_unknown_releases_locks_for_pre_and_post_registration() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    let pre = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.rec",
            "h1",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "rec-pre",
        ))
        .unwrap();
    let claim_pre = store
        .claim_staged_plugin_operation(pre.operation.operation_id, pre.operation.revision, 0, 10)
        .unwrap();
    let unk_pre = store
        .cancel_plugin_operation_admin(pre.operation.operation_id, claim_pre.revision(), 11)
        .unwrap();
    assert_eq!(unk_pre.state, PluginOperationState::Unknown);
    let settled_pre = store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unk_pre.operation_id,
            revision: unk_pre.revision,
            generation: unk_pre.generation,
            attempt: claim_pre.attempt(),
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    assert_eq!(settled_pre.state, PluginOperationState::Cancelled);
    assert_eq!(
        store
            .get_active_plugin_operation("org.example.rec")
            .unwrap(),
        None
    );

    let post = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.rec",
            "h1",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "rec-post",
        ))
        .unwrap();
    let mut claim_post = store
        .claim_staged_plugin_operation(post.operation.operation_id, post.operation.revision, 0, 20)
        .unwrap();
    let reg = store
        .register_staged_plugin_version(
            &mut claim_post,
            &version_meta("org.example.rec", "h1", "1.0.0"),
            &post.intent.grants,
            21,
        )
        .unwrap();
    assert_eq!(reg.active_operation_id, Some(post.operation.operation_id));
    let unk_post = store
        .cancel_plugin_operation_admin(post.operation.operation_id, claim_post.revision(), 22)
        .unwrap();
    let settled_post = store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unk_post.operation_id,
            revision: unk_post.revision,
            generation: unk_post.generation,
            attempt: claim_post.attempt(),
            outcome: PluginReconciliationOutcome::Failed,
        })
        .unwrap();
    assert_eq!(settled_post.state, PluginOperationState::Failed);
    let unlocked = store.get_plugin("org.example.rec").unwrap().unwrap();
    assert_eq!(unlocked.active_operation_id, None);
}

#[test]
fn reconcile_non_staged_enabling_op_preserves_active_operation_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&version_meta("org.example.nonstg", "h1", "1.0.0"), 0)
        .unwrap();
    let en_op = accept_mgmt_op(&store, &p.plugin_id, p.revision, "enable", "non-staged-en");
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, en_op.operation_id)
        .unwrap();
    let en_claim = store
        .claim_plugin_command(en_op.operation_id, en_op.revision, None, 0, 30)
        .unwrap();
    let en_unk = store
        .record_plugin_command_interruption(
            en_op.operation_id,
            en_claim.revision(),
            None,
            en_claim.attempt(),
        )
        .unwrap();
    store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: en_unk.operation_id,
            revision: en_unk.revision,
            generation: en_unk.generation,
            attempt: en_claim.attempt(),
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    let still_enabling = store.get_plugin(&p.plugin_id).unwrap().unwrap();
    assert_eq!(still_enabling.lifecycle, PluginLifecycle::Enabling);
    assert_eq!(still_enabling.active_operation_id, Some(en_op.operation_id));
    let enabled = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: p.plugin_id,
                operation_id: en_op.operation_id,
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(enabled.lifecycle, PluginLifecycle::Enabled);
    assert_eq!(enabled.active_operation_id, None);
}

#[test]
fn recover_rejects_cancel_requested_and_register_rejects_hash_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    let op1 = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.guards",
            "h1",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "g-cancel-rec",
        ))
        .unwrap();
    let claim1 = store
        .claim_staged_plugin_operation(op1.operation.operation_id, op1.operation.revision, 0, 10)
        .unwrap();
    let unk1 = store
        .cancel_plugin_operation_admin(op1.operation.operation_id, claim1.revision(), 11)
        .unwrap();
    assert_eq!(unk1.phase, "cancel-requested");
    assert!(matches!(
        store.recover_staged_plugin_operation(
            unk1.operation_id,
            unk1.revision,
            claim1.attempt(),
            "staging",
            12,
        ),
        Err(StoreError::Plugin(
            PluginError::UnknownEffect | PluginError::Conflict
        ))
    ));
    let still_unk = store
        .get_plugin_operation_admin(unk1.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(still_unk.operation.state, PluginOperationState::Unknown);
    assert_eq!(still_unk.operation.effect, PluginEffectObservation::Unknown);
    store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unk1.operation_id,
            revision: unk1.revision,
            generation: unk1.generation,
            attempt: claim1.attempt(),
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();

    let op2 = store
        .begin_staged_plugin_operation(&staged_accept(
            "org.example.guards",
            "h1",
            "1.0.0",
            StagedPluginOpKind::Install,
            0,
            "g-hash-mismatch",
        ))
        .unwrap();
    let mut claim2 = store
        .claim_staged_plugin_operation(op2.operation.operation_id, op2.operation.revision, 0, 20)
        .unwrap();
    assert!(matches!(
        store.register_staged_plugin_version(
            &mut claim2,
            &version_meta("org.example.guards", "h-WRONG", "1.0.0"),
            &[],
            21,
        ),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));
    assert_eq!(store.get_plugin("org.example.guards").unwrap(), None);
}

#[test]
fn cancel_admin_rejects_active_non_staged_enabling_or_draining_lifecycle_op() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&version_meta("org.example.adm", "h1", "1.0.0"), 0)
        .unwrap();

    let en_op = accept_mgmt_op(&store, &p.plugin_id, p.revision, "enable", "g-enabling");
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, en_op.operation_id)
        .unwrap();
    assert!(matches!(
        store.cancel_plugin_operation_admin(en_op.operation_id, en_op.revision, 30),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let enabled = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: p.plugin_id.clone(),
                operation_id: en_op.operation_id,
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();

    let dis_op = accept_mgmt_op(
        &store,
        &enabled.plugin_id,
        enabled.revision,
        "disable",
        "g-draining",
    );
    let _draining = store
        .begin_plugin_lifecycle(
            &enabled.plugin_id,
            enabled.revision,
            dis_op.operation_id,
            false,
        )
        .unwrap();
    assert!(matches!(
        store.cancel_plugin_operation_admin(dis_op.operation_id, dis_op.revision, 31),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
}

#[test]
fn non_admin_cancel_on_queued_staged_op_releases_locks_and_unblocks_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let mut pre_input = staged_accept(
        "org.example.nonadm",
        "h1",
        "1.0.0",
        StagedPluginOpKind::Install,
        0,
        "non-adm-pre",
    );
    let subj = pre_input.subject.clone();
    let pre_inst = store
        .begin_staged_plugin_operation(&pre_input)
        .unwrap()
        .operation;
    let cancelled_pre = store
        .cancel_plugin_operation(&subj, pre_inst.operation_id, pre_inst.revision)
        .unwrap();
    assert_eq!(cancelled_pre.state, PluginOperationState::Cancelled);
    assert_eq!(
        store
            .get_active_plugin_operation("org.example.nonadm")
            .unwrap(),
        None
    );
    pre_input.idempotency_key = "non-adm-pre-2".into();
    pre_input.cause = PluginCommandCause::root("cause-non-adm-pre-2");
    let retry = store.begin_staged_plugin_operation(&pre_input).unwrap();
    store
        .cancel_plugin_operation_admin(retry.operation.operation_id, retry.operation.revision, 10)
        .unwrap();

    let p = store
        .register_plugin_version(&version_meta("org.example.nonadm", "h1", "1.0.0"), 0)
        .unwrap();
    let mut reset_input = staged_accept(
        &p.plugin_id,
        "h1",
        "1.0.0",
        StagedPluginOpKind::StateReset,
        p.revision,
        "non-adm-reset",
    );
    reset_input.subject = subj.clone();
    reset_input.intent.package_hash = None;
    reset_input.intent.version = None;
    reset_input.intent.state_policy = Some("reset".into());
    reset_input.intent.grants = vec![];
    let reset = store
        .begin_staged_plugin_operation(&reset_input)
        .unwrap()
        .operation;
    let cancelled_reset = store
        .cancel_plugin_operation(&subj, reset.operation_id, reset.revision)
        .unwrap();
    assert_eq!(cancelled_reset.state, PluginOperationState::Cancelled);
    let after = store.get_plugin(&p.plugin_id).unwrap().unwrap();
    assert_eq!(after.active_operation_id, None);
    store
        .uninstall_plugin(&after.plugin_id, after.revision, None, true)
        .unwrap();
}
