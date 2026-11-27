use std::sync::{Arc, mpsc};
use store::{Db, Store, StoreError, plugins::*};

fn meta(id: &str, hash: &str) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: hash.into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "/pkg/a".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
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

#[test]
fn plugins_page_and_config_summary_enforce_bounds_redaction_and_reader_pool() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    for i in 0..55 {
        store
            .register_plugin_version(
                &meta(&format!("org.example.p{i:03}"), &format!("h-{i:03}")),
                0,
            )
            .unwrap();
    }
    let page1 = store.list_plugins_page(None, None, None).unwrap();
    assert_eq!(page1.items.len(), 50);
    assert!(page1.next_cursor.is_some());
    let page2 = store
        .list_plugins_page(None, page1.next_cursor.as_deref(), None)
        .unwrap();
    assert_eq!(page2.items.len(), 5);
    assert_eq!(page2.next_cursor, None);

    let p = store.get_plugin("org.example.p000").unwrap().unwrap();
    let cfg_auth = store.issue_plugin_config_authority(&p, None).unwrap();
    let p = store
        .save_plugin_config_with_secrets(
            &cfg_auth,
            p.revision,
            "1",
            &serde_json::json!({"public":true}),
            &[PluginSecretUpdate {
                key: "api_secret".into(),
                change: PluginSecretChange::Replace(b"super-secret-plaintext-token".to_vec()),
            }],
        )
        .unwrap();
    let enabled = enable_plugin(&store, &p);
    let summary = store.get_plugin_config_summary(&enabled.plugin_id).unwrap();
    assert_eq!(summary.secrets.len(), 1);
    assert!(summary.secrets[0].configured);
    assert_eq!(summary.secrets[0].key, "api_secret");
    assert!(!format!("{summary:?}").contains("super-secret-plaintext-token"));

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
    assert_eq!(
        store
            .list_plugins_page(Some(PluginLifecycle::Enabled), None, None)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        store
            .get_plugin_config_summary(&enabled.plugin_id)
            .unwrap()
            .secrets
            .len(),
        1
    );
    assert!(matches!(
        store.get_plugin_config_summary("missing-plugin"),
        Err(StoreError::Plugin(PluginError::NotFound))
    ));
    tx_release.send(()).unwrap();
    handle.join().unwrap();
}

#[test]
fn task_bindings_operations_and_audit_queries_return_redacted_pages() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&meta("org.example.ops", "h-ops"), 0)
        .unwrap();
    let enabled = enable_plugin(&store, &p);
    store
        .save_plugin_task_binding(
            &enabled,
            "task-a",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(60),
                public_config: serde_json::json!({}),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .list_plugin_task_bindings(&enabled.plugin_id, None, None)
            .unwrap()
            .items
            .len(),
        1
    );

    let accepted = store
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(enabled.plugin_id.clone()),
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "audit-1".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/org.example.ops/enable".into(),
                body_digest: [2; 32],
            },
            kind: "enable".into(),
            expected_revision: Some(enabled.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("audit-cause"),
        })
        .unwrap();
    let ops = store
        .list_plugin_operations_admin(Some(&enabled.plugin_id), None, None, None)
        .unwrap();
    assert_eq!(ops.items.len(), 1);
    assert_eq!(ops.items[0].operation_id, accepted.operation.operation_id);

    let audit = store
        .list_plugin_audit_events(&enabled.plugin_id, Some("enable"), None, None, None)
        .unwrap();
    assert_eq!(audit.items.len(), 1);
    assert!(
        store
            .list_plugin_task_receipts(&enabled.plugin_id, "task-a", None, None)
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn task_receipts_audit_cursor_and_secret_debug_redaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();
    let p = store
        .register_plugin_version(&meta("org.example.rec", "h-rec"), 0)
        .unwrap();
    let p = store
        .grant_plugin_permissions(
            &p.plugin_id,
            &p.package_hash,
            p.revision,
            &[
                PluginGrant {
                    capability: "kv".into(),
                    scope: PluginGrantScope::Global,
                    sensitive: false,
                },
                PluginGrant {
                    capability: "command:sync".into(),
                    scope: PluginGrantScope::Global,
                    sensitive: false,
                },
            ],
        )
        .unwrap();
    let op_en = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, op_en)
        .unwrap();
    let enabled = store
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
    let state_auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    store
        .plugin_state_put(&state_auth, PluginStateKind::Kv, "k", b"1234", 0)
        .unwrap();
    let preview = store
        .preview_plugin_state_reset(&enabled.plugin_id, None)
        .unwrap();
    assert_eq!(preview.kv_entries, 1);
    assert_eq!(preview.kv_bytes, 5);

    let cmd_auth = store
        .authorize_plugin_command(&state_auth, "sync", &[PluginCommandTarget::Global])
        .unwrap();
    for i in 0..5 {
        store
            .accept_plugin_command(
                &cmd_auth,
                &AcceptPluginOperation {
                    plugin_id: Some(enabled.plugin_id.clone()),
                    subject: PluginSubject::Task {
                        plugin_id: enabled.plugin_id.clone(),
                        task_id: "daily".into(),
                    },
                    idempotency_key: format!("run-{i}"),
                    request: CanonicalPluginRequest {
                        method: "POST".into(),
                        path: "/tasks/daily/run".into(),
                        body_digest: [i as u8; 32],
                    },
                    kind: "sync".into(),
                    expected_revision: Some(enabled.revision),
                    expected_generation: Some(enabled.generation),
                    cause: PluginCommandCause::root(format!("c-{i}")),
                },
                b"secret-command-payload",
            )
            .unwrap();
    }

    let r_page1 = store
        .list_plugin_task_receipts(&enabled.plugin_id, "daily", None, Some(2))
        .unwrap();
    assert_eq!(r_page1.items.len(), 2);
    assert!(r_page1.next_cursor.is_some());
    let after_id = uuid::Uuid::parse_str(r_page1.next_cursor.as_deref().unwrap()).unwrap();
    let r_page2 = store
        .list_plugin_task_receipts(&enabled.plugin_id, "daily", Some(after_id), Some(10))
        .unwrap();
    assert_eq!(r_page2.items.len(), 3);
    assert_eq!(r_page2.next_cursor, None);

    let a_page1 = store
        .list_plugin_audit_events(&enabled.plugin_id, None, None, None, Some(3))
        .unwrap();
    assert_eq!(a_page1.items.len(), 3);
    assert!(a_page1.next_cursor.is_some());
    let a_page2 = store
        .list_plugin_audit_events(
            &enabled.plugin_id,
            None,
            None,
            a_page1.next_cursor.as_deref(),
            Some(10),
        )
        .unwrap();
    assert_eq!(a_page2.items.len(), 2);
    assert_eq!(a_page2.next_cursor, None);

    let secret_update = PluginSecretUpdate {
        key: "token".into(),
        change: PluginSecretChange::Replace(b"my-raw-secret-value".to_vec()),
    };
    assert!(!format!("{secret_update:?}").contains("my-raw-secret-value"));
}
