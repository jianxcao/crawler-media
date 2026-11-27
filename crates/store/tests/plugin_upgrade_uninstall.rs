use store::{Store, StoreError, plugins::*};

fn make_version(
    id: &str,
    hash: &str,
    ver: &str,
    schema: &str,
    key_id: Option<&str>,
    sig: &str,
) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: hash.into(),
        version: ver.into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: schema.into(),
        artifact_path: format!("/pkg/{id}/{hash}.cmplugin"),
        signing_key_id: key_id.map(Into::into),
        signature_status: sig.into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

fn enable_dev(store: &Store, p: &PluginRecord) -> PluginRecord {
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

fn disable_dev(store: &Store, p: &PluginRecord) -> PluginRecord {
    let op = uuid::Uuid::new_v4();
    let draining = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, op, false)
        .unwrap();
    store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: p.plugin_id.clone(),
                operation_id: op,
                revision: draining.revision,
                generation: draining.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap()
}

fn seed_user_subscribe(store: &Store, user: domain::UserId) {
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: "Show".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    store
        .insert_subscribe(&domain::Subscribe {
            id: domain::SubscribeId::new(),
            user_id: user,
            media_id,
            coverage: domain::Coverage::Movie,
            fetch_mode: domain::FetchMode::Search,
            filter_id: domain::FilterId::new(),
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
}

fn install_kv_plugin(store: &Store, id: &str, hash: &str, ver: &str) -> PluginRecord {
    let p = store
        .register_plugin_version(&make_version(id, hash, ver, "1", None, "development"), 0)
        .unwrap();
    store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[PluginGrant {
                capability: "kv".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }],
        )
        .unwrap()
}

#[test]
fn uninstall_retain_private_data_preserves_kv_and_subscribes_and_allows_reinstall() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let user = domain::UserId::new();
    seed_user_subscribe(&store, user);

    let p = install_kv_plugin(&store, "org.example.uninst", "h1", "1.0.0");
    let enabled = enable_dev(&store, &p);
    let auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    store
        .plugin_state_put(&auth, PluginStateKind::Kv, "cursor", b"v1", 0)
        .unwrap();

    assert!(matches!(
        store.uninstall_plugin(&enabled.plugin_id, enabled.revision, None, false),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    let disabled = disable_dev(&store, &enabled);

    let op = store
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(disabled.plugin_id.clone()),
            subject: PluginSubject::User(user),
            idempotency_key: "op-keep".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/org.example.uninst/uninstall".into(),
                body_digest: [1; 32],
            },
            kind: "uninstall".into(),
            expected_revision: Some(disabled.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("c1"),
        })
        .unwrap();

    assert!(matches!(
        store.uninstall_plugin(&disabled.plugin_id, disabled.revision, None, false),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    let claim = store
        .claim_plugin_command(
            op.operation.operation_id,
            op.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();
    store
        .record_plugin_command_outcome(
            &claim,
            &PluginCommandOutcome {
                state: PluginOperationState::Succeeded,
                phase: "done".into(),
                next_run_at: None,
                result: None,
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .unwrap();

    let out = store
        .uninstall_plugin(&disabled.plugin_id, disabled.revision, None, false)
        .unwrap();
    assert!(!out.private_data_removed);
    assert!(out.unreferenced_package_hashes.contains(&"h1".to_string()));
    assert_eq!(store.get_plugin(&disabled.plugin_id).unwrap(), None);
    assert!(
        store
            .get_plugin_operation_admin(op.operation.operation_id)
            .unwrap()
            .is_some()
    );
    assert!(
        !store
            .list_plugin_audit_events(&disabled.plugin_id, None, None, None, None)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(store.list_all_subscribes().unwrap().len(), 1);

    let re_enabled = enable_dev(
        &store,
        &install_kv_plugin(&store, "org.example.uninst", "h2", "1.1.0"),
    );
    let re_auth = store
        .issue_plugin_state_authority(&re_enabled, None)
        .unwrap();
    assert_eq!(
        store
            .plugin_state_get(&re_auth, PluginStateKind::Kv, "cursor")
            .unwrap()
            .unwrap()
            .value
            .as_bytes(),
        b"v1"
    );
}

#[test]
fn uninstall_staged_with_remove_private_data_wipes_kv_and_preserves_subscribes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let user = domain::UserId::new();
    seed_user_subscribe(&store, user);

    let enabled = enable_dev(
        &store,
        &install_kv_plugin(&store, "org.example.wipe", "h1", "1.0.0"),
    );
    let auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    store
        .plugin_state_put(&auth, PluginStateKind::Kv, "cursor", b"v1", 0)
        .unwrap();
    let disabled = disable_dev(&store, &enabled);

    let uninst_op = store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(user),
            idempotency_key: "op-wipe".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/org.example.wipe/uninstall".into(),
                body_digest: [2; 32],
            },
            op_kind: StagedPluginOpKind::Uninstall,
            expected_revision: disabled.revision,
            intent: StagedPluginIntent {
                target_plugin_id: disabled.plugin_id.clone(),
                package_hash: None,
                version: None,
                artifact_path: None,
                state_policy: None,
                target_config_digest: None,
                grants: vec![],
            },
            cause: PluginCommandCause::root("c2"),
        })
        .unwrap();
    let claim_wipe = store
        .claim_staged_plugin_operation(
            uninst_op.operation.operation_id,
            uninst_op.operation.revision,
            0,
            20,
        )
        .unwrap();
    let out_wipe = store
        .uninstall_plugin(
            &disabled.plugin_id,
            disabled.revision,
            Some(uninst_op.operation.operation_id),
            true,
        )
        .unwrap();
    assert!(out_wipe.private_data_removed);
    store
        .complete_staged_plugin_operation(
            &claim_wipe,
            PluginOperationState::Succeeded,
            "uninstalled",
            None,
            None,
            21,
        )
        .unwrap();
    assert_eq!(store.list_all_subscribes().unwrap().len(), 1);
}

fn kv_grant() -> PluginGrant {
    PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }
}

#[test]
fn upgrade_preserve_keeps_kv_and_reset_enforces_schema_policy_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let grant = kv_grant();
    let p = install_kv_plugin(&store, "org.example.upg", "h1", "1.0.0");
    let en = enable_dev(&store, &p);
    let auth = store.issue_plugin_state_authority(&en, None).unwrap();
    store
        .plugin_state_put(&auth, PluginStateKind::Kv, "k", b"val", 0)
        .unwrap();
    let dis = disable_dev(&store, &en);

    // Same state_schema "1" -> Preserve keeps KV while switching active_package_hash
    let dis = store
        .register_plugin_version(
            &make_version("org.example.upg", "h1b", "1.1.0", "1", None, "development"),
            dis.revision,
        )
        .unwrap();
    let preserved = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: dis.plugin_id.clone(),
            expected_revision: dis.revision,
            operation_id: None,
            target_package_hash: "h1b".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![grant.clone()],
        })
        .unwrap();
    assert_eq!(preserved.package_hash, "h1b");
    let en_p = enable_dev(&store, &preserved);
    let auth_p = store.issue_plugin_state_authority(&en_p, None).unwrap();
    assert_eq!(
        store
            .plugin_state_get(&auth_p, PluginStateKind::Kv, "k")
            .unwrap()
            .unwrap()
            .value
            .as_bytes(),
        b"val"
    );
    let dis = disable_dev(&store, &en_p);

    // Different state_schema "s2" -> Preserve fails, leaving KV intact
    let dis = store
        .register_plugin_version(
            &make_version("org.example.upg", "h2", "2.0.0", "s2", None, "development"),
            dis.revision,
        )
        .unwrap();
    assert!(matches!(
        store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: dis.plugin_id.clone(),
            expected_revision: dis.revision,
            operation_id: None,
            target_package_hash: "h2".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![grant.clone()],
        }),
        Err(StoreError::Plugin(PluginError::StateSchemaMismatch))
    ));

    // Reset succeeds atomically, switching hash and clearing KV/quota
    let switched = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: dis.plugin_id.clone(),
            expected_revision: dis.revision,
            operation_id: None,
            target_package_hash: "h2".into(),
            state_policy: PluginStatePolicy::Reset { scopes: None },
            target_config: Some(PluginUpgradeConfig {
                schema_revision: "2".into(),
                values: serde_json::json!({"mode":"v2"}),
                secret_scope_updates: vec![],
            }),
            target_grants: vec![grant],
        })
        .unwrap();
    assert_eq!(switched.package_hash, "h2");
    assert_eq!(switched.config_revision, 1);
    let en2 = enable_dev(&store, &switched);
    let auth2 = store.issue_plugin_state_authority(&en2, None).unwrap();
    assert_eq!(
        store
            .plugin_state_get(&auth2, PluginStateKind::Kv, "k")
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .plugin_state_quota(&auth2, PluginStateKind::Kv)
            .unwrap()
            .plugin_kv_bytes,
        0
    );
}

#[test]
fn standalone_state_reset_and_rollback_enforce_trust_and_atomicity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let grant = kv_grant();
    let p = install_kv_plugin(&store, "org.example.rst", "h1", "1.0.0");
    let en = enable_dev(&store, &p);
    let auth = store.issue_plugin_state_authority(&en, None).unwrap();
    store
        .plugin_state_put(&auth, PluginStateKind::Kv, "k2", b"abc", 0)
        .unwrap();
    let dis = disable_dev(&store, &en);

    // Rollback inside app_write preserves KV & plugin pointer
    assert!(
        store
            .app_write(|app| {
                app.reset_plugin_state(
                    &dis.plugin_id,
                    dis.revision,
                    None,
                    Some(&[auth.security_scope().clone()]),
                )?;
                Err::<(), _>(StoreError::Invalid("abort".into()))
            })
            .is_err()
    );
    let reset = store
        .reset_plugin_state(
            &dis.plugin_id,
            dis.revision,
            None,
            Some(&[auth.security_scope().clone()]),
        )
        .unwrap();
    assert_eq!(reset.deleted_kv_rows, 1);
    let en2 = enable_dev(&store, &reset.plugin);
    let auth2 = store.issue_plugin_state_authority(&en2, None).unwrap();
    assert_eq!(
        store
            .plugin_state_quota(&auth2, PluginStateKind::Kv)
            .unwrap()
            .plugin_kv_bytes,
        0
    );

    // Rollback to signed older version is blocked if its signing key was revoked
    let dis2 = disable_dev(&store, &en2);
    store
        .pin_plugin_trust_key(&PinPluginTrustKey {
            key_id: "old-key".into(),
            label: "Old".into(),
            public_key: "pk-old".into(),
            fingerprint: "fp-old".into(),
            actor: "admin".into(),
        })
        .unwrap();
    let dis2 = store
        .register_plugin_version(
            &make_version(
                "org.example.rst",
                "h0",
                "0.9.0",
                "1",
                Some("old-key"),
                "signed",
            ),
            dis2.revision,
        )
        .unwrap();
    store
        .revoke_plugin_trust_key("old-key", Some(1), "revoked", "admin")
        .unwrap();
    assert!(matches!(
        store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: dis2.plugin_id.clone(),
            expected_revision: dis2.revision,
            operation_id: None,
            target_package_hash: "h0".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![grant],
        }),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
}

#[test]
fn replace_dev_plugin_version_validates_grants_schema_drain_signing_key_and_fences_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let pid = "org.example.devreload";
    let p = install_kv_plugin(&store, pid, "h-dev-1", "1.0.0");
    let enabled = enable_dev(&store, &p);
    store
        .save_plugin_task_binding(
            &enabled,
            "sync",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(120),
                public_config: serde_json::json!({}),
            },
        )
        .unwrap();
    let disabled = disable_dev(&store, &enabled);

    let signed_dev = make_version(pid, "h-dev-key", "1.0.1", "1", Some("k1"), "development");
    assert!(matches!(
        store.replace_dev_plugin_version(&signed_dev, &[]),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));

    let valid_v2 = make_version(pid, "h-dev-2", "1.0.1", "1", None, "development");
    assert!(matches!(
        store.replace_dev_plugin_version(
            &valid_v2,
            &[PluginGrant {
                capability: "site:*".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }],
        ),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));

    let mismatch_schema = make_version(pid, "h-dev-schema", "1.0.1", "2", None, "development");
    assert!(matches!(
        store.replace_dev_plugin_version(&mismatch_schema, &[]),
        Err(StoreError::Plugin(PluginError::StateSchemaMismatch))
    ));

    let pending_op = store
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.into()),
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "dev-blocking-op".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/org.example.devreload/config".into(),
                body_digest: [9; 32],
            },
            kind: "configure".into(),
            expected_revision: Some(disabled.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("dev-op"),
        })
        .unwrap();
    assert!(matches!(
        store.replace_dev_plugin_version(&valid_v2, &[kv_grant()]),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));

    let claim = store
        .claim_plugin_command(
            pending_op.operation.operation_id,
            pending_op.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();
    store
        .record_plugin_command_outcome(
            &claim,
            &PluginCommandOutcome {
                state: PluginOperationState::Succeeded,
                phase: "done".into(),
                next_run_at: None,
                result: None,
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .unwrap();

    let reloaded = store
        .replace_dev_plugin_version(&valid_v2, &[kv_grant()])
        .unwrap();
    assert_eq!(reloaded.package_hash, "h-dev-2");
    assert_eq!(reloaded.revision, disabled.revision + 1);
    assert_eq!(reloaded.generation, disabled.generation + 1);
    let binding = store.get_plugin_task_binding(pid, "sync").unwrap().unwrap();
    assert!(!binding.definition.enabled);
    assert_eq!(binding.generation, reloaded.generation);
}

fn accept_sync_cmd(
    store: &Store,
    cmd_auth: &PluginCommandAuthority,
    enabled: &PluginRecord,
    idem: &str,
    byte: u8,
    payload: &[u8],
) -> AcceptedPluginOperation {
    store
        .accept_plugin_command(
            cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some(enabled.plugin_id.clone()),
                subject: PluginSubject::User(domain::UserId::new()),
                idempotency_key: idem.into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{}/commands/sync", enabled.plugin_id),
                    body_digest: [byte; 32],
                },
                kind: "sync".into(),
                expected_revision: Some(enabled.revision),
                expected_generation: Some(enabled.generation),
                cause: PluginCommandCause::root(format!("c-{idem}")),
            },
            payload,
        )
        .unwrap()
}

#[test]
fn uninstall_with_remove_private_data_purges_command_payloads_and_operation_results() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let pid = "org.example.purge-ops";
    let p = store
        .register_plugin_version(
            &make_version(pid, "h1", "1.0.0", "1", None, "development"),
            0,
        )
        .unwrap();
    let p = store
        .grant_plugin_permissions(
            pid,
            "h1",
            p.revision,
            &[PluginGrant {
                capability: "command:sync".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }],
        )
        .unwrap();
    let enabled = enable_dev(&store, &p);
    let state_auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    let cmd_auth = store
        .authorize_plugin_command(&state_auth, "sync", &[PluginCommandTarget::Global])
        .unwrap();
    let accepted = accept_sync_cmd(
        &store,
        &cmd_auth,
        &enabled,
        "cmd-purge-1",
        7,
        br#"{"secret_input":"raw-payload"}"#,
    );
    let claim = store
        .claim_plugin_command(
            accepted.operation.operation_id,
            accepted.operation.revision,
            accepted.operation.generation,
            0,
            10,
        )
        .unwrap();
    store
        .record_plugin_command_outcome(
            &claim,
            &PluginCommandOutcome {
                state: PluginOperationState::Succeeded,
                phase: "done".into(),
                next_run_at: None,
                result: Some(br#"{"secret_output":"raw-result"}"#.to_vec()),
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .unwrap();
    let queued = accept_sync_cmd(
        &store,
        &cmd_auth,
        &enabled,
        "cmd-purge-2",
        8,
        br#"{"secret_input":"queued-payload"}"#,
    );

    let disabled = disable_dev(&store, &enabled);
    assert!(
        store
            .get_plugin_operation_admin(accepted.operation.operation_id)
            .unwrap()
            .unwrap()
            .result_json
            .is_some()
    );

    store
        .uninstall_plugin(pid, disabled.revision, None, true)
        .unwrap();

    let admin_after = store
        .get_plugin_operation_admin(accepted.operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(admin_after.result_json, None);
    assert!(
        store
            .get_plugin_operation_admin(queued.operation.operation_id)
            .unwrap()
            .is_some()
    );
    let (payload_rows, result_rows): (i64, i64) = store
        .with_read(store::Db::App, |conn| {
            let p_cnt: i64 =
                conn.query_row("SELECT COUNT(*) FROM plugin_command_payloads", [], |r| {
                    r.get(0)
                })?;
            let r_cnt: i64 =
                conn.query_row("SELECT COUNT(*) FROM plugin_operation_results", [], |r| {
                    r.get(0)
                })?;
            Ok((p_cnt, r_cnt))
        })
        .unwrap();
    assert_eq!((payload_rows, result_rows), (0, 0));
}
