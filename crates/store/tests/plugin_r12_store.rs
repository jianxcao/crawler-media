#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{Store, StoreError, plugins::*};

fn fill_kv_to_limit(store: &Store, auth: &PluginStateAuthority) {
    let limit: i64 = 5 * 1024 * 1024;
    let chunk = vec![5u8; 65536];
    for i in 0..79 {
        store
            .plugin_state_put(auth, PluginStateKind::Kv, &format!("fill{i:02}"), &chunk, 0)
            .unwrap();
    }
    let used = store
        .plugin_state_quota(auth, PluginStateKind::Kv)
        .unwrap()
        .plugin_kv_bytes;
    let tail = vec![4u8; (limit - used - 4) as usize];
    store
        .plugin_state_put(auth, PluginStateKind::Kv, "tail", &tail, 0)
        .unwrap();
    assert_eq!(
        store
            .plugin_state_quota(auth, PluginStateKind::Kv)
            .unwrap()
            .plugin_kv_bytes,
        limit
    );
}

fn version_meta(id: &str, ver: &str, hash: &str, bound: bool) -> PluginVersionMetadata {
    let mut m = metadata(id, bound);
    m.version = ver.into();
    m.package_hash = hash.into();
    m
}

#[test]
fn switch_and_dev_reload_enforce_credential_bound_domain_and_purge_cross_domain_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "test").unwrap();
    let s = site();
    store.insert_site(&s).unwrap();

    // Install v1 (`credential_bound = false`), save Global secret, exhaust KV quota, then disable.
    let p_init = install(&store, "org.example.domain-flip", &[]);
    let cfg_global = store.issue_plugin_config_authority(&p_init, None).unwrap();
    let p_cfg = store
        .save_plugin_config_with_secrets(
            &cfg_global,
            p_init.revision,
            "1",
            &serde_json::json!({}),
            &[PluginSecretUpdate {
                key: "token".into(),
                change: PluginSecretChange::Replace(b"global-sec".to_vec()),
            }],
        )
        .unwrap();
    let p_en = enable(&store, &p_cfg);
    let state_global = store.issue_plugin_state_authority(&p_en, None).unwrap();
    fill_kv_to_limit(&store, &state_global);
    let p_dis = disable(&store, &p_en);

    // Dev reload flipping `credential_bound` (`false -> true`, same `state_schema = "1"`) must fail.
    let dev_flip = version_meta("org.example.domain-flip", "1.0.1", "dev-hash-2", true);
    assert!(matches!(
        store.replace_dev_plugin_version(&dev_flip, &[]),
        Err(StoreError::Plugin(PluginError::StateSchemaMismatch))
    ));

    // Register v2 (`credential_bound = true`) and verify Preserve & partial Reset are rejected.
    let v2 = version_meta("org.example.domain-flip", "2.0.0", "hash-v2", true);
    let p_reg = store.register_plugin_version(&v2, p_dis.revision).unwrap();
    let site_grants = vec![PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Site(s.id),
        sensitive: false,
    }];
    for bad_policy in [
        PluginStatePolicy::Preserve,
        PluginStatePolicy::Reset {
            scopes: Some(vec![PluginSecurityScope::global()]),
        },
    ] {
        assert!(matches!(
            store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
                plugin_id: p_reg.plugin_id.clone(),
                expected_revision: p_reg.revision,
                operation_id: None,
                target_package_hash: v2.package_hash.clone(),
                state_policy: bad_policy,
                target_config: None,
                target_grants: site_grants.clone(),
            }),
            Err(StoreError::Plugin(PluginError::StateSchemaMismatch))
        ));
    }

    // Full reset succeeds, clears KV quota, and purges old Global secret so v3 (`false`) sees none.
    let p_v2 = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: p_reg.plugin_id.clone(),
            expected_revision: p_reg.revision,
            operation_id: None,
            target_package_hash: v2.package_hash.clone(),
            state_policy: PluginStatePolicy::Reset { scopes: None },
            target_config: None,
            target_grants: site_grants,
        })
        .unwrap();
    let p_v2_en = enable(&store, &p_v2);
    let site_auth = store
        .issue_plugin_state_authority(&p_v2_en, store.observe_site(s.id).unwrap().as_ref())
        .unwrap();
    store
        .plugin_state_put(&site_auth, PluginStateKind::Kv, "site-key", b"ok", 0)
        .expect("site KV write must succeed after full reset across credential_bound flip");

    let p_v2_dis = disable(&store, &p_v2_en);
    let v3 = version_meta("org.example.domain-flip", "3.0.0", "hash-v3", false);
    let p_v3_reg = store
        .register_plugin_version(&v3, p_v2_dis.revision)
        .unwrap();
    let _p_v3 = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: p_v3_reg.plugin_id.clone(),
            expected_revision: p_v3_reg.revision,
            operation_id: None,
            target_package_hash: v3.package_hash.clone(),
            state_policy: PluginStatePolicy::Reset { scopes: None },
            target_config: None,
            target_grants: vec![],
        })
        .unwrap();
    assert!(
        store
            .get_plugin_config_summary("org.example.domain-flip")
            .unwrap()
            .secrets
            .is_empty(),
        "prior Global secret must not resurrect after flipping false -> true -> false"
    );
}

#[test]
fn reinstall_after_keeping_private_data_purges_incompatible_domain_quota() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "test").unwrap();
    let s = site();
    store.insert_site(&s).unwrap();

    // 1. `false -> true` via `register_plugin_version` after `uninstall_plugin(..., false)`
    let p_v1 = enable(&store, &install(&store, "org.example.reinstall", &[]));
    let g_auth = store.issue_plugin_state_authority(&p_v1, None).unwrap();
    fill_kv_to_limit(&store, &g_auth);
    let p_v1_dis = disable(&store, &p_v1);
    store
        .uninstall_plugin(&p_v1_dis.plugin_id, p_v1_dis.revision, None, false)
        .unwrap();

    let p_v2 = enable(
        &store,
        &install(&store, "org.example.reinstall", std::slice::from_ref(&s)),
    );
    let s_auth = store
        .issue_plugin_state_authority(&p_v2, store.observe_site(s.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(
        store
            .plugin_state_quota(&s_auth, PluginStateKind::Kv)
            .unwrap()
            .plugin_kv_bytes,
        0
    );
    fill_kv_to_limit(&store, &s_auth);

    // 2. `true -> false` via `register_staged_plugin_version` after `uninstall_plugin(..., false)`
    let p_v2_dis = disable(&store, &p_v2);
    store
        .uninstall_plugin(&p_v2_dis.plugin_id, p_v2_dis.revision, None, false)
        .unwrap();

    let v3 = version_meta("org.example.reinstall", "3.0.0", "staged-v3", false);
    let grants = vec![PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let accepted = store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "reinstall-v3".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/install".into(),
                body_digest: [9; 32],
            },
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: v3.plugin_id.clone(),
                package_hash: Some(v3.package_hash.clone()),
                version: Some(v3.version.clone()),
                artifact_path: Some(v3.artifact_path.clone()),
                state_policy: None,
                target_config_digest: None,
                grants: grants.clone(),
            },
            cause: PluginCommandCause::root("reinstall-v3"),
        })
        .unwrap();
    let mut claim = store
        .claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            100,
        )
        .unwrap();
    let _p_staged = store
        .register_staged_plugin_version(&mut claim, &v3, &grants, 101)
        .unwrap();
    store
        .complete_staged_plugin_operation(
            &claim,
            PluginOperationState::Succeeded,
            "completed",
            None,
            None,
            102,
        )
        .unwrap();
    let p_v3 = enable(&store, &store.get_plugin(&v3.plugin_id).unwrap().unwrap());
    let g_auth_v3 = store.issue_plugin_state_authority(&p_v3, None).unwrap();
    assert_eq!(
        store
            .plugin_state_quota(&g_auth_v3, PluginStateKind::Kv)
            .unwrap()
            .plugin_kv_bytes,
        0
    );
    store
        .plugin_state_put(&g_auth_v3, PluginStateKind::Kv, "global-ok", b"1", 0)
        .expect("global KV write must succeed after staged reinstall flipping true -> false");
}

#[test]
fn switch_validates_secret_scopes_and_cross_entry_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "test").unwrap();
    let s = site();
    store.insert_site(&s).unwrap();
    let obs1 = store.observe_site(s.id).unwrap().unwrap();
    let stale_scope = PluginSecurityScope::for_site(&obs1);
    store
        .save_site(&s, store::ManualCredentialWrite::Replace)
        .unwrap();
    let live_scope = PluginSecurityScope::for_site(&store.observe_site(s.id).unwrap().unwrap());

    let p_v1 = install(&store, "org.example.switch-sec", std::slice::from_ref(&s));
    let v2 = version_meta("org.example.switch-sec", "2.0.0", "sec-v2", true);
    let p_reg = store.register_plugin_version(&v2, p_v1.revision).unwrap();

    // 1. Duplicate `(scope, key)` across separate entries in `secret_scope_updates` -> Invalid.
    assert!(matches!(
        store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: p_reg.plugin_id.clone(),
            expected_revision: p_reg.revision,
            operation_id: None,
            target_package_hash: v2.package_hash.clone(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: Some(PluginUpgradeConfig {
                schema_revision: "1".into(),
                values: serde_json::json!({}),
                secret_scope_updates: vec![
                    (
                        live_scope.clone(),
                        vec![PluginSecretUpdate {
                            key: "api_key".into(),
                            change: PluginSecretChange::Replace(b"a".to_vec()),
                        }],
                    ),
                    (
                        live_scope.clone(),
                        vec![PluginSecretUpdate {
                            key: "api_key".into(),
                            change: PluginSecretChange::Clear,
                        }],
                    ),
                ],
            }),
            target_grants: vec![],
        }),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));

    // 2. Wrong domain (`Global` on `credential_bound = true`) -> Denied.
    assert!(matches!(
        store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: p_reg.plugin_id.clone(),
            expected_revision: p_reg.revision,
            operation_id: None,
            target_package_hash: v2.package_hash.clone(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: Some(PluginUpgradeConfig {
                schema_revision: "1".into(),
                values: serde_json::json!({}),
                secret_scope_updates: vec![(
                    PluginSecurityScope::global(),
                    vec![PluginSecretUpdate {
                        key: "api_key".into(),
                        change: PluginSecretChange::Replace(b"a".to_vec()),
                    }],
                )],
            }),
            target_grants: vec![],
        }),
        Err(StoreError::Plugin(PluginError::Denied))
    ));

    // 3. Retired site credential_epoch -> StaleAuthority.
    assert!(matches!(
        store.apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: p_reg.plugin_id.clone(),
            expected_revision: p_reg.revision,
            operation_id: None,
            target_package_hash: v2.package_hash.clone(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: Some(PluginUpgradeConfig {
                schema_revision: "1".into(),
                values: serde_json::json!({}),
                secret_scope_updates: vec![(
                    stale_scope,
                    vec![PluginSecretUpdate {
                        key: "api_key".into(),
                        change: PluginSecretChange::Replace(b"a".to_vec()),
                    }],
                )],
            }),
            target_grants: vec![],
        }),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
}
