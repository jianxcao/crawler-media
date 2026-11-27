use store::{Store, StoreError, plugins::*};

fn make_version(id: &str, hash: &str, ver: &str) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: hash.into(),
        version: ver.into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: format!("/pkg/{id}/{hash}.cmplugin"),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

fn kv_grant() -> PluginGrant {
    PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }
}

fn install_and_enable(store: &Store, id: &str, hash: &str, ver: &str) -> PluginRecord {
    let p = store
        .register_plugin_version(&make_version(id, hash, ver), 0)
        .unwrap();
    let cmd_grant = PluginGrant {
        capability: "command:sync.run".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let p = store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[kv_grant(), cmd_grant],
        )
        .unwrap();
    let op = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, op)
        .unwrap();
    store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: p.plugin_id.clone(),
                operation_id: op,
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap()
}

fn accept_runtime_cmd(store: &Store, p: &PluginRecord, key: &str) -> PluginOperation {
    let auth = store.issue_plugin_state_authority(p, None).unwrap();
    let cmd_auth = store
        .authorize_plugin_command(&auth, "sync.run", &[PluginCommandTarget::Global])
        .unwrap();
    store
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some(p.plugin_id.clone()),
                subject: PluginSubject::User(domain::UserId::new()),
                idempotency_key: key.into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{}/run", p.plugin_id),
                    body_digest: [1; 32],
                },
                kind: "sync.run".into(),
                expected_revision: Some(p.revision),
                expected_generation: Some(p.generation),
                cause: PluginCommandCause::root(format!("cause-{key}")),
            },
            b"",
        )
        .unwrap()
        .operation
}

fn accept_staged_switch(
    store: &Store,
    p: &PluginRecord,
    op_kind: StagedPluginOpKind,
    hash: &str,
    ver: &str,
    key: &str,
) -> (PluginOperation, PluginCommandClaim) {
    let accepted = store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: key.into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{}/switch", p.plugin_id),
                body_digest: [3; 32],
            },
            op_kind,
            expected_revision: p.revision,
            intent: StagedPluginIntent {
                target_plugin_id: p.plugin_id.clone(),
                package_hash: Some(hash.into()),
                version: Some(ver.into()),
                artifact_path: None,
                state_policy: Some("preserve".into()),
                target_config_digest: None,
                grants: vec![kv_grant()],
            },
            cause: PluginCommandCause::root(format!("cause-{key}")),
        })
        .unwrap();
    let claim = store
        .claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            20,
        )
        .unwrap();
    (accepted.operation, claim)
}

fn op_row(store: &Store, id: uuid::Uuid) -> PluginOperation {
    store
        .get_plugin_operation_admin(id)
        .unwrap()
        .unwrap()
        .operation
}

#[test]
fn staged_upgrade_drains_enabled_plugin_and_preserves_lock_until_switch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let en = install_and_enable(&store, "org.example.stgupg", "h1", "1.0.0");

    let queued_rt = accept_runtime_cmd(&store, &en, "rt-queued");
    let running_rt = accept_runtime_cmd(&store, &en, "rt-running");
    let rt_claim = store
        .claim_plugin_command(
            running_rt.operation_id,
            running_rt.revision,
            running_rt.generation,
            0,
            10,
        )
        .unwrap();

    let (upg, mut upg_claim) = accept_staged_switch(
        &store,
        &en,
        StagedPluginOpKind::Upgrade,
        "h2",
        "2.0.0",
        "u1",
    );
    assert!(matches!(
        store.begin_plugin_lifecycle(&en.plugin_id, en.revision, upg.operation_id, true),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let draining = store
        .begin_plugin_lifecycle(&en.plugin_id, en.revision, upg.operation_id, false)
        .unwrap();
    assert_eq!(draining.lifecycle, PluginLifecycle::Draining);
    assert_eq!(draining.active_operation_id, Some(upg.operation_id));
    assert_eq!(
        op_row(&store, upg.operation_id).state,
        PluginOperationState::Running
    );
    assert_eq!(
        op_row(&store, queued_rt.operation_id).state,
        PluginOperationState::Cancelled
    );
    let rt_unk = op_row(&store, running_rt.operation_id);
    assert_eq!(rt_unk.state, PluginOperationState::Unknown);

    let obs = PluginLifecycleObservation {
        plugin_id: en.plugin_id.clone(),
        operation_id: upg.operation_id,
        revision: draining.revision,
        generation: draining.generation,
        workers_exited: true,
        unknown_effects_reconciled: true,
        validation_succeeded: true,
    };
    assert!(matches!(
        store.finalize_plugin_lifecycle_checked(&obs, None),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: rt_unk.operation_id,
            revision: rt_unk.revision,
            generation: rt_unk.generation,
            attempt: rt_claim.attempt(),
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    let drained = store.finalize_plugin_lifecycle_checked(&obs, None).unwrap();
    assert_eq!(drained.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(drained.active_operation_id, Some(upg.operation_id));

    let drained = store
        .register_staged_plugin_version(
            &mut upg_claim,
            &make_version("org.example.stgupg", "h2", "2.0.0"),
            std::slice::from_ref(&kv_grant()),
            30,
        )
        .unwrap();
    let switched = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: drained.plugin_id.clone(),
            expected_revision: drained.revision,
            operation_id: Some(upg.operation_id),
            target_package_hash: "h2".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![kv_grant()],
        })
        .unwrap();
    assert_eq!(switched.package_hash, "h2");
    assert_eq!(switched.active_operation_id, Some(upg.operation_id));
    store
        .complete_staged_plugin_operation(
            &upg_claim,
            PluginOperationState::Succeeded,
            "upgraded",
            None,
            None,
            31,
        )
        .unwrap();
    assert_eq!(
        store
            .get_plugin(&en.plugin_id)
            .unwrap()
            .unwrap()
            .active_operation_id,
        None
    );
}

#[test]
fn staged_rollback_drains_enabled_plugin_and_completes_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&make_version("org.example.stgrb", "h1", "1.0.0"), 0)
        .unwrap();
    let p = store
        .register_plugin_version(
            &make_version("org.example.stgrb", "h2", "2.0.0"),
            p.revision,
        )
        .unwrap();
    let p = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: p.plugin_id.clone(),
            expected_revision: p.revision,
            operation_id: None,
            target_package_hash: "h2".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![kv_grant()],
        })
        .unwrap();
    let op_en = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, op_en)
        .unwrap();
    let en = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: p.plugin_id.clone(),
                operation_id: op_en,
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();

    let (rb, rb_claim) = accept_staged_switch(
        &store,
        &en,
        StagedPluginOpKind::Rollback,
        "h1",
        "1.0.0",
        "r1",
    );
    let draining = store
        .begin_plugin_lifecycle(&en.plugin_id, en.revision, rb.operation_id, false)
        .unwrap();
    let drained = store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: en.plugin_id.clone(),
            operation_id: rb.operation_id,
            revision: draining.revision,
            generation: draining.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    assert_eq!(drained.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(drained.active_operation_id, Some(rb.operation_id));
    let switched = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: drained.plugin_id.clone(),
            expected_revision: drained.revision,
            operation_id: Some(rb.operation_id),
            target_package_hash: "h1".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![kv_grant()],
        })
        .unwrap();
    assert_eq!(switched.package_hash, "h1");
    store
        .complete_staged_plugin_operation(
            &rb_claim,
            PluginOperationState::Succeeded,
            "rolled-back",
            None,
            None,
            41,
        )
        .unwrap();
    assert_eq!(
        store
            .get_plugin(&en.plugin_id)
            .unwrap()
            .unwrap()
            .active_operation_id,
        None
    );
}

#[test]
fn fenced_ops_reject_stale_operation_id_and_preview_deduplicates_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let en = install_and_enable(&store, "org.example.polish", "h1", "1.0.0");
    let auth = store.issue_plugin_state_authority(&en, None).unwrap();
    store
        .plugin_state_put(&auth, PluginStateKind::Kv, "k1", b"1234567", 0)
        .unwrap();
    let dis_op = uuid::Uuid::new_v4();
    let draining = store
        .begin_plugin_lifecycle(&en.plugin_id, en.revision, dis_op, false)
        .unwrap();
    let dis = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: en.plugin_id.clone(),
                operation_id: dis_op,
                revision: draining.revision,
                generation: draining.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    let dis = store
        .register_plugin_version(
            &make_version("org.example.polish", "h2", "2.0.0"),
            dis.revision,
        )
        .unwrap();
    assert_eq!(dis.active_operation_id, None);

    let preview = store
        .preview_plugin_state_reset(
            &dis.plugin_id,
            Some(&[auth.security_scope().clone(), auth.security_scope().clone()]),
        )
        .unwrap();
    assert_eq!(preview.kv_entries, 1);
    assert_eq!(preview.kv_bytes, 9);

    let stale = Some(uuid::Uuid::new_v4());
    assert!(matches!(
        store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: dis.plugin_id.clone(),
            expected_revision: dis.revision,
            operation_id: stale,
            target_package_hash: "h2".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![kv_grant()],
        }),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.reset_plugin_state(&dis.plugin_id, dis.revision, stale, None),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.uninstall_plugin(&dis.plugin_id, dis.revision, stale, false),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
}

#[test]
fn staged_upgrade_interrupted_and_reconciled_during_draining_finishes_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let en = install_and_enable(&store, "org.example.drnrec", "h1", "1.0.0");

    let (upg, upg_claim) = accept_staged_switch(
        &store,
        &en,
        StagedPluginOpKind::Upgrade,
        "h2",
        "2.0.0",
        "drn-rec-1",
    );
    let draining = store
        .begin_plugin_lifecycle(&en.plugin_id, en.revision, upg.operation_id, false)
        .unwrap();
    assert_eq!(draining.lifecycle, PluginLifecycle::Draining);
    assert_eq!(draining.active_operation_id, Some(upg.operation_id));

    assert!(matches!(
        store.cancel_plugin_operation_admin(upg.operation_id, upg_claim.revision(), 25),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.complete_staged_plugin_operation(
            &upg_claim,
            PluginOperationState::Failed,
            "failed-early",
            None,
            Some("aborted"),
            26,
        ),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));

    let unk = store
        .record_plugin_command_interruption(
            upg.operation_id,
            upg_claim.revision(),
            upg.generation,
            upg_claim.attempt(),
        )
        .unwrap();
    let reconciled = store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unk.operation_id,
            revision: unk.revision,
            generation: unk.generation,
            attempt: upg_claim.attempt(),
            outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
    assert_eq!(reconciled.state, PluginOperationState::Cancelled);

    let still_draining = store.get_plugin(&en.plugin_id).unwrap().unwrap();
    assert_eq!(still_draining.lifecycle, PluginLifecycle::Draining);
    assert_eq!(still_draining.active_operation_id, Some(upg.operation_id));

    let disabled = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: en.plugin_id.clone(),
                operation_id: upg.operation_id,
                revision: still_draining.revision,
                generation: still_draining.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(disabled.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(disabled.active_operation_id, None);

    let re_enable_op = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&disabled.plugin_id, disabled.revision, re_enable_op)
        .unwrap();
    assert_eq!(snap.plugin.lifecycle, PluginLifecycle::Enabling);
}

fn accept_simple_staged(
    store: &Store,
    subj: PluginSubject,
    id: &str,
    op_kind: StagedPluginOpKind,
    expected_revision: i64,
    pkg: Option<(&str, &str)>,
    key: &str,
) -> PluginOperation {
    store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: subj,
            idempotency_key: key.into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{id}/op"),
                body_digest: [5; 32],
            },
            op_kind,
            expected_revision,
            intent: StagedPluginIntent {
                target_plugin_id: id.into(),
                package_hash: pkg.map(|(h, _)| h.into()),
                version: pkg.map(|(_, v)| v.into()),
                artifact_path: None,
                state_policy: Some("preserve".into()),
                target_config_digest: None,
                grants: vec![kv_grant()],
            },
            cause: PluginCommandCause::root(format!("cause-{key}")),
        })
        .unwrap()
        .operation
}

#[test]
fn queued_staged_upgrade_and_failed_reconcile_in_draining_do_not_brick_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let en1 = install_and_enable(&store, "org.example.drnq", "h1", "1.0.0");
    let subj = PluginSubject::User(domain::UserId::new());
    let upg1 = accept_simple_staged(
        &store,
        subj.clone(),
        &en1.plugin_id,
        StagedPluginOpKind::Upgrade,
        en1.revision,
        Some(("h2", "2.0.0")),
        "drnq-1",
    );
    let draining1 = store
        .begin_plugin_lifecycle(&en1.plugin_id, en1.revision, upg1.operation_id, false)
        .unwrap();
    assert!(matches!(
        store.cancel_plugin_operation_admin(upg1.operation_id, upg1.revision, 30),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.cancel_plugin_operation(&subj, upg1.operation_id, upg1.revision),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let disabled1 = store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: en1.plugin_id.clone(),
            operation_id: upg1.operation_id,
            revision: draining1.revision,
            generation: draining1.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    assert_eq!(disabled1.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(disabled1.active_operation_id, Some(upg1.operation_id));

    let en2 = install_and_enable(&store, "org.example.drnf", "h1", "1.0.0");
    let (upg2, upg2_claim) = accept_staged_switch(
        &store,
        &en2,
        StagedPluginOpKind::Upgrade,
        "h2",
        "2.0.0",
        "drnf-1",
    );
    let draining2 = store
        .begin_plugin_lifecycle(&en2.plugin_id, en2.revision, upg2.operation_id, false)
        .unwrap();
    let unk2 = store
        .record_plugin_command_interruption(
            upg2.operation_id,
            upg2_claim.revision(),
            upg2.generation,
            upg2_claim.attempt(),
        )
        .unwrap();
    store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: unk2.operation_id,
            revision: unk2.revision,
            generation: unk2.generation,
            attempt: upg2_claim.attempt(),
            outcome: PluginReconciliationOutcome::Failed,
        })
        .unwrap();
    let disabled2 = store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: en2.plugin_id.clone(),
            operation_id: upg2.operation_id,
            revision: draining2.revision,
            generation: draining2.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    assert_eq!(disabled2.lifecycle, PluginLifecycle::Disabled);
    assert_eq!(disabled2.active_operation_id, None);
    store
        .uninstall_plugin(&disabled2.plugin_id, disabled2.revision, None, true)
        .unwrap();
}

#[test]
fn begin_plugin_enable_checked_rejects_staged_install_uninstall_and_upgrade_ops() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let subj = PluginSubject::User(domain::UserId::new());

    let inst = accept_simple_staged(
        &store,
        subj.clone(),
        "org.example.enrej",
        StagedPluginOpKind::Install,
        0,
        Some(("h1", "1.0.0")),
        "en-rej-inst",
    );
    let mut inst_claim = store
        .claim_staged_plugin_operation(inst.operation_id, inst.revision, 0, 10)
        .unwrap();
    let reg = store
        .register_staged_plugin_version(
            &mut inst_claim,
            &make_version("org.example.enrej", "h1", "1.0.0"),
            &[kv_grant()],
            11,
        )
        .unwrap();
    assert!(matches!(
        store.begin_plugin_enable_checked(&reg.plugin_id, reg.revision, inst.operation_id),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    store
        .complete_staged_plugin_operation(
            &inst_claim,
            PluginOperationState::Succeeded,
            "installed",
            None,
            None,
            12,
        )
        .unwrap();

    let reg = store.get_plugin("org.example.enrej").unwrap().unwrap();
    let uninst = accept_simple_staged(
        &store,
        subj,
        &reg.plugin_id,
        StagedPluginOpKind::Uninstall,
        reg.revision,
        None,
        "en-rej-uninst",
    );
    assert!(matches!(
        store.begin_plugin_enable_checked(&reg.plugin_id, reg.revision, uninst.operation_id),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    store
        .cancel_plugin_operation_admin(uninst.operation_id, uninst.revision, 13)
        .unwrap();

    let reg = store.get_plugin("org.example.enrej").unwrap().unwrap();
    let en_op = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&reg.plugin_id, reg.revision, en_op)
        .unwrap();
    let en = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: reg.plugin_id.clone(),
                operation_id: en_op,
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    let (upg, _upg_claim) = accept_staged_switch(
        &store,
        &en,
        StagedPluginOpKind::Upgrade,
        "h2",
        "2.0.0",
        "en-rej-upg",
    );
    let draining = store
        .begin_plugin_lifecycle(&en.plugin_id, en.revision, upg.operation_id, false)
        .unwrap();
    let drained = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: en.plugin_id.clone(),
                operation_id: upg.operation_id,
                revision: draining.revision,
                generation: draining.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    assert!(matches!(
        store.begin_plugin_enable_checked(&drained.plugin_id, drained.revision, upg.operation_id),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
}
