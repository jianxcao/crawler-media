use store::{Store, StoreError, plugins::*};

fn version(id: &str, hash: &str, key_id: Option<&str>, sig_status: &str) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: hash.into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "/never-read/pkg".into(),
        signing_key_id: key_id.map(Into::into),
        signature_status: sig_status.into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

#[test]
fn checked_enable_enforces_developer_mode_and_trust_key_policy() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    // Unsigned plugin cannot enable when developer mode is off
    let dev = store
        .register_plugin_version(&version("org.example.dev", "h-dev", None, "development"), 0)
        .unwrap();
    assert!(matches!(
        store.begin_plugin_enable_checked(&dev.plugin_id, dev.revision, uuid::Uuid::new_v4()),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let op_dev = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&dev.plugin_id, dev.revision, op_dev)
        .unwrap();
    assert!(snap.developer_mode_enabled);
    assert_eq!(snap.plugin.lifecycle, PluginLifecycle::Enabling);
    assert_eq!(store.list_active_unsigned_plugins().unwrap().len(), 1);

    let enabled_dev = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: dev.plugin_id.clone(),
                operation_id: op_dev,
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(enabled_dev.lifecycle, PluginLifecycle::Enabled);

    // Signed plugin fails without trusted key, succeeds when pinned, refuses finalize if revoked mid-enable
    store
        .pin_plugin_trust_key(&PinPluginTrustKey {
            key_id: "key-1".into(),
            label: "K1".into(),
            public_key: "pk1".into(),
            fingerprint: "fp1".into(),
            actor: "admin".into(),
        })
        .unwrap();
    let signed = store
        .register_plugin_version(
            &version("org.example.signed", "h-sig", Some("key-1"), "signed"),
            0,
        )
        .unwrap();
    let op_sig = uuid::Uuid::new_v4();
    let snap_sig = store
        .begin_plugin_enable_checked(&signed.plugin_id, signed.revision, op_sig)
        .unwrap();
    assert_eq!(snap_sig.signing_key_active, Some(true));
    assert_eq!(
        store
            .list_active_plugins_for_trust_key("key-1")
            .unwrap()
            .len(),
        1
    );

    // Revoke key while Enabling -> finalize refuses activation and leaves Disabled with failure_reason
    store
        .revoke_plugin_trust_key("key-1", Some(1), "compromised", "admin")
        .unwrap();
    let refused = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: signed.plugin_id.clone(),
                operation_id: op_sig,
                revision: snap_sig.plugin.revision,
                generation: snap_sig.plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(refused.lifecycle, PluginLifecycle::Disabled);
    assert!(!refused.desired_enabled);
    assert!(refused.failure_reason.is_some());
    assert!(
        store
            .list_active_plugins_for_trust_key("key-1")
            .unwrap()
            .is_empty()
    );
}

fn enable_plugin(store: &Store, p: &PluginRecord) -> PluginRecord {
    let op_en = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, op_en)
        .unwrap();
    store
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
        .unwrap()
}

fn accept_revoke_op(store: &Store, p: &PluginRecord, key: &str) -> uuid::Uuid {
    store
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(p.plugin_id.clone()),
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: key.into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{}/revoke", p.plugin_id),
                body_digest: [3; 32],
            },
            kind: "revoke-permissions".into(),
            expected_revision: Some(p.revision),
            expected_generation: None,
            cause: PluginCommandCause::root(format!("cause-{key}")),
        })
        .unwrap()
        .operation
        .operation_id
}

#[test]
fn grant_and_revoke_permissions_transition_enabled_plugin_to_draining() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&version("org.example.perms", "h-p", None, "development"), 0)
        .unwrap();
    let kv_grant = PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let blob_grant = PluginGrant {
        capability: "blob".into(),
        scope: PluginGrantScope::Global,
        sensitive: true,
    };
    let p = store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            std::slice::from_ref(&kv_grant),
        )
        .unwrap();
    let p = store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            std::slice::from_ref(&blob_grant),
        )
        .unwrap();
    assert_eq!(store.get_plugin_grants(&p.plugin_id).unwrap().len(), 2);

    let enabled = enable_plugin(&store, &p);
    let auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    let binding = store
        .save_plugin_task_binding(
            &enabled,
            "sync",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(60),
                public_config: serde_json::json!({}),
            },
        )
        .unwrap();
    assert!(binding.definition.enabled);

    // Revoking while Enabled atomically revokes grant, transitions to Draining, and disables task bindings
    let revoke_op = accept_revoke_op(&store, &enabled, "revoke-blob");
    let draining = store
        .revoke_plugin_permissions(
            &enabled.plugin_id,
            &enabled.package_hash,
            enabled.revision,
            std::slice::from_ref(&blob_grant),
            Some(revoke_op),
        )
        .unwrap();
    assert_eq!(draining.lifecycle, PluginLifecycle::Draining);
    assert_eq!(draining.active_operation_id, Some(revoke_op));
    assert!(draining.generation > enabled.generation);
    assert!(matches!(
        store.validate_plugin_state_authority(&auth),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    let task_after = store
        .get_plugin_task_binding(&enabled.plugin_id, "sync")
        .unwrap()
        .unwrap();
    assert!(!task_after.definition.enabled);

    let all_grants = store
        .list_plugin_grants(&enabled.plugin_id, &enabled.package_hash)
        .unwrap();
    assert_eq!(all_grants.len(), 2);
    assert_eq!(
        all_grants
            .iter()
            .find(|g| g.grant.capability == "blob")
            .unwrap()
            .status,
        PluginGrantStatus::Revoked
    );
    assert_eq!(
        all_grants
            .iter()
            .find(|g| g.grant.capability == "kv")
            .unwrap()
            .status,
        PluginGrantStatus::Granted
    );
}

#[test]
fn revoke_permissions_while_enabled_rejects_invalid_or_runtime_operation_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let kv_grant = PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let cmd_grant = PluginGrant {
        capability: "command:sync.run".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    };
    let p = store
        .register_plugin_version(&version("org.example.revval", "h1", None, "development"), 0)
        .unwrap();
    let p = store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[kv_grant.clone(), cmd_grant],
        )
        .unwrap();
    let en = enable_plugin(&store, &p);

    // Non-existent UUID rejected
    assert!(matches!(
        store.revoke_plugin_permissions(
            &en.plugin_id,
            &en.package_hash,
            en.revision,
            std::slice::from_ref(&kv_grant),
            Some(uuid::Uuid::new_v4()),
        ),
        Err(StoreError::Plugin(PluginError::NotFound))
    ));

    // Runtime command UUID rejected
    let auth = store.issue_plugin_state_authority(&en, None).unwrap();
    let cmd_auth = store
        .authorize_plugin_command(&auth, "sync.run", &[PluginCommandTarget::Global])
        .unwrap();
    let rt = store
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some(en.plugin_id.clone()),
                subject: PluginSubject::User(domain::UserId::new()),
                idempotency_key: "rt-1".into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: "/plugins/org.example.revval/run".into(),
                    body_digest: [1; 32],
                },
                kind: "sync.run".into(),
                expected_revision: Some(en.revision),
                expected_generation: Some(en.generation),
                cause: PluginCommandCause::root("c-rt"),
            },
            b"",
        )
        .unwrap();
    assert!(matches!(
        store.revoke_plugin_permissions(
            &en.plugin_id,
            &en.package_hash,
            en.revision,
            std::slice::from_ref(&kv_grant),
            Some(rt.operation.operation_id),
        ),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    // Other plugin's management operation rejected
    let other = store
        .register_plugin_version(&version("org.example.other", "h2", None, "development"), 0)
        .unwrap();
    let other_op = accept_revoke_op(&store, &other, "other-op");
    assert!(matches!(
        store.revoke_plugin_permissions(
            &en.plugin_id,
            &en.package_hash,
            en.revision,
            std::slice::from_ref(&kv_grant),
            Some(other_op),
        ),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
}

#[test]
fn semantic_grant_scopes_reject_whitespace_invalid_site_and_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = store
        .register_plugin_version(
            &version("org.example.sem1", "h-sem1", None, "development"),
            0,
        )
        .unwrap();
    let sid = domain::SiteId::new();

    for bad_scope in [
        PluginGrantScope::Object {
            kind: "channel".into(),
            id: "   ".into(),
        },
        PluginGrantScope::Object {
            kind: "   ".into(),
            id: "ch-1".into(),
        },
        PluginGrantScope::Object {
            kind: "site".into(),
            id: "  site:   ".into(),
        },
    ] {
        assert!(matches!(
            store.grant_plugin_permissions(
                &p.plugin_id,
                &p.package_hash,
                p.revision,
                &[PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: bad_scope,
                    sensitive: true,
                }],
            ),
            Err(StoreError::Plugin(PluginError::Invalid(_)))
        ));
    }

    assert!(matches!(
        store.grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Site(sid),
                    sensitive: true,
                },
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Object {
                        kind: "site".into(),
                        id: format!("  SITE:{}  ", sid.to_string().to_ascii_uppercase()),
                    },
                    sensitive: true,
                },
            ],
        ),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));
}

#[test]
fn semantic_grant_scopes_revoke_and_regrant_symmetrically() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&version("org.example.sem", "h-sem", None, "development"), 0)
        .unwrap();
    let sid = domain::SiteId::new();
    let dl_id = domain::DownloaderId::new();
    let alt_site_scope = PluginGrantScope::Object {
        kind: "site".into(),
        id: format!("  SITE:{}  ", sid.to_string().to_ascii_uppercase()),
    };

    let p = store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: alt_site_scope,
                    sensitive: true,
                },
                PluginGrant {
                    capability: "command:downloader.add".into(),
                    scope: PluginGrantScope::Object {
                        kind: "downloader".into(),
                        id: format!("  DOWNLOADER:{}  ", dl_id.to_string().to_ascii_uppercase()),
                    },
                    sensitive: false,
                },
            ],
        )
        .unwrap();
    let p = store
        .revoke_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Site(sid),
                    sensitive: true,
                },
                PluginGrant {
                    capability: "command:downloader.add".into(),
                    scope: PluginGrantScope::Object {
                        kind: "downloader".into(),
                        id: dl_id.to_string(),
                    },
                    sensitive: false,
                },
            ],
            None,
        )
        .unwrap();
    assert!(store.get_plugin_grants(&p.plugin_id).unwrap().is_empty());
    let en = enable_plugin(&store, &p);
    let auth = store.issue_plugin_state_authority(&en, None).unwrap();
    assert!(matches!(
        store.authorize_plugin_command(
            &auth,
            "site.maintenance",
            &[PluginCommandTarget::Site(sid)],
        ),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
}

#[test]
fn grant_validation_rejects_padded_or_uppercase_object_kind_and_capability_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = store
        .register_plugin_version(
            &version("org.example.r6grant", "h-r6grant", None, "development"),
            0,
        )
        .unwrap();
    let sid = domain::SiteId::new();

    for bad in [
        PluginGrant {
            capability: " kv ".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "command:site.maintenance".into(),
            scope: PluginGrantScope::Object {
                kind: " site ".into(),
                id: sid.to_string(),
            },
            sensitive: true,
        },
        PluginGrant {
            capability: "command:site.maintenance".into(),
            scope: PluginGrantScope::Object {
                kind: "SITE".into(),
                id: sid.to_string(),
            },
            sensitive: true,
        },
    ] {
        assert!(matches!(
            store.grant_plugin_permissions(&p.plugin_id, &p.package_hash, p.revision, &[bad]),
            Err(StoreError::Plugin(PluginError::Invalid(_)))
        ));
    }
}

#[test]
fn advance_staged_phase_guards_intent_identity_and_grants() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let intent = StagedPluginIntent {
        target_plugin_id: "org.example.r6a".into(),
        package_hash: Some("hash-r6a".into()),
        version: Some("1.0.0".into()),
        artifact_path: None,
        state_policy: None,
        target_config_digest: None,
        grants: vec![],
    };
    let begin = store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "r6-stg-1".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/api/v1/plugin-installations".into(),
                body_digest: [7; 32],
            },
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            cause: PluginCommandCause::root("r6-stg"),
            intent: intent.clone(),
        })
        .unwrap();
    let mut claim = store
        .claim_staged_plugin_operation(
            begin.operation.operation_id,
            begin.operation.revision,
            0,
            11,
        )
        .unwrap();

    // Mutating target_plugin_id or package_hash via advance_staged_plugin_phase must be rejected
    let mut wrong_target = intent.clone();
    wrong_target.target_plugin_id = "org.example.r6b".into();
    assert!(matches!(
        store.advance_staged_plugin_phase(&mut claim, "staging", Some(&wrong_target), 12),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));
    let mut wrong_hash = intent.clone();
    wrong_hash.package_hash = Some("hash-other".into());
    assert!(matches!(
        store.advance_staged_plugin_phase(&mut claim, "staging", Some(&wrong_hash), 12),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));
    let mut bad_grants = intent;
    bad_grants.grants = vec![PluginGrant {
        capability: "kv*".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    assert!(matches!(
        store.advance_staged_plugin_phase(&mut claim, "staging", Some(&bad_grants), 12),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));
}
