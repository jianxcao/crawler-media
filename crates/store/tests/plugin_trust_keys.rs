use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::{Arc, mpsc};
use store::{Db, Store, StoreError, plugins::*};

fn app_hash(dir: &std::path::Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(dir.join("app.db")).unwrap()).to_vec()
}

#[test]
fn plugin_trust_keys_schema_and_phase2_rejection_preserve_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened.list_plugin_trust_keys(None, None).unwrap().items,
        vec![]
    );
    drop(reopened);

    for bad_sql in [
        "DROP INDEX plugin_trust_keys_status",
        "DROP TABLE plugin_trust_keys",
        "CREATE TABLE plugin_repos(id TEXT PRIMARY KEY)",
        "DROP TABLE plugin_ui_trust",
        "CREATE TABLE unexpected_future_table(id TEXT PRIMARY KEY)",
    ] {
        let case_dir = tempfile::tempdir().unwrap();
        drop(Store::open(case_dir.path()).unwrap());
        let conn = Connection::open(case_dir.path().join("app.db")).unwrap();
        conn.execute_batch(bad_sql).unwrap();
        drop(conn);
        let before = app_hash(case_dir.path());
        assert!(matches!(
            Store::open(case_dir.path()),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(app_hash(case_dir.path()), before);
    }
}

#[test]
fn trust_key_pin_list_package_count_and_idempotent_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pinned = store
        .pin_plugin_trust_key(&PinPluginTrustKey {
            key_id: "RWQ-key-01".into(),
            label: "Official Release Key".into(),
            public_key: "RWRlc3RQdWJsaWNLZXlNYXRlcmlhbDAx".into(),
            fingerprint: "sha256:fp-01".into(),
            actor: "admin-1".into(),
        })
        .unwrap();
    assert_eq!(pinned.status, PluginTrustKeyStatus::Trusted);
    assert_eq!(pinned.revision, 1);
    assert_eq!(pinned.revoked_at, None);

    // Duplicate key_id or duplicate fingerprint -> Conflict
    assert!(matches!(
        store.pin_plugin_trust_key(&PinPluginTrustKey {
            key_id: "RWQ-key-01".into(),
            label: "Dup ID".into(),
            public_key: "pk-2".into(),
            fingerprint: "sha256:fp-02".into(),
            actor: "admin-1".into(),
        }),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.pin_plugin_trust_key(&PinPluginTrustKey {
            key_id: "RWQ-key-02".into(),
            label: "Dup FP".into(),
            public_key: "pk-3".into(),
            fingerprint: "sha256:fp-01".into(),
            actor: "admin-1".into(),
        }),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    // Invalid empty field -> Invalid
    assert!(matches!(
        store.pin_plugin_trust_key(&PinPluginTrustKey {
            key_id: "".into(),
            label: "Empty".into(),
            public_key: "pk".into(),
            fingerprint: "fp".into(),
            actor: "admin-1".into(),
        }),
        Err(StoreError::Plugin(PluginError::Invalid(_)))
    ));

    // Register a signed version referencing this key to check associated_package_count
    store
        .register_plugin_version(
            &PluginVersionMetadata {
                plugin_id: "org.example.signed".into(),
                package_hash: "hash-signed-1".into(),
                version: "1.0.0".into(),
                manifest_json: "{}".into(),
                config_schema_json: "{}".into(),
                state_schema: "1".into(),
                artifact_path: "/never-read/signed.cmplugin".into(),
                signing_key_id: Some("RWQ-key-01".into()),
                signature_status: "signed".into(),
                ui_metadata_ref: None,
                credential_bound: false,
            },
            0,
        )
        .unwrap();

    let listed = store.list_plugin_trust_keys(None, None).unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].associated_package_count, 1);
    assert_eq!(
        store.count_plugin_versions_for_key("RWQ-key-01").unwrap(),
        1
    );

    // Revoke with stale revision fails
    assert!(matches!(
        store.revoke_plugin_trust_key("RWQ-key-01", Some(99), "stale", "admin-2"),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.revoke_plugin_trust_key("missing-key", None, "none", "admin-2"),
        Err(StoreError::Plugin(PluginError::NotFound))
    ));

    let revoked = store
        .revoke_plugin_trust_key("RWQ-key-01", Some(1), "key rotated", "admin-2")
        .unwrap();
    assert_eq!(revoked.status, PluginTrustKeyStatus::Revoked);
    assert_eq!(revoked.revision, 2);
    assert!(revoked.revoked_at.is_some());
    assert_eq!(revoked.revoked_by.as_deref(), Some("admin-2"));
    assert_eq!(revoked.revoked_reason.as_deref(), Some("key rotated"));

    // Idempotent second revoke preserves original metadata
    let again = store
        .revoke_plugin_trust_key("RWQ-key-01", None, "second reason", "admin-3")
        .unwrap();
    assert_eq!(again, revoked);
    assert!(
        store
            .list_active_plugin_trust_keys(None, None)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store.get_plugin_trust_key("RWQ-key-01").unwrap(),
        Some(revoked)
    );
}

#[test]
fn trust_key_pagination_defaults_to_50_and_clamps_to_200() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for i in 0..205 {
        store
            .pin_plugin_trust_key(&PinPluginTrustKey {
                key_id: format!("key-{i:03}"),
                label: format!("Key {i}"),
                public_key: format!("pk-{i}"),
                fingerprint: format!("fp-{i}"),
                actor: "admin".into(),
            })
            .unwrap();
    }
    let page_default = store.list_plugin_trust_keys(None, None).unwrap();
    assert_eq!(page_default.items.len(), 50);
    assert_eq!(page_default.next_cursor.as_deref(), Some("key-049"));

    let page_max = store.list_plugin_trust_keys(None, Some(999)).unwrap();
    assert_eq!(page_max.items.len(), 200);
    assert_eq!(page_max.next_cursor.as_deref(), Some("key-199"));

    let page_rest = store
        .list_active_plugin_trust_keys(page_max.next_cursor.as_deref(), Some(200))
        .unwrap();
    assert_eq!(page_rest.items.len(), 5);
    assert_eq!(page_rest.next_cursor, None);
}

#[test]
fn developer_mode_persists_cas_and_reads_bypass_writer_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let initial = store.plugin_developer_mode().unwrap();
    assert!(!initial.enabled);
    assert_eq!(initial.revision, 0);

    let enabled = store.set_plugin_developer_mode(true, 0, "admin-1").unwrap();
    assert!(enabled.enabled);
    assert_eq!(enabled.revision, 1);
    assert_eq!(enabled.updated_by.as_deref(), Some("admin-1"));
    assert!(enabled.updated_at.is_some());

    assert!(matches!(
        store.set_plugin_developer_mode(false, 0, "admin-1"),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert!(matches!(
        store.set_plugin_developer_mode(false, -1, "admin-1"),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    // Rollback inside app_write
    assert!(
        store
            .app_write(|app| {
                app.set_plugin_developer_mode(false, 1, "admin-1")?;
                Err::<(), _>(StoreError::Invalid("abort".into()))
            })
            .is_err()
    );
    assert_eq!(
        Store::open(dir.path())
            .unwrap()
            .plugin_developer_mode()
            .unwrap(),
        enabled
    );

    // ReaderPool reads succeed while Writers::app() is held on another thread
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
    assert!(store.plugin_developer_mode().unwrap().enabled);
    assert_eq!(
        store.list_plugin_trust_keys(None, None).unwrap().items,
        vec![]
    );
    tx_release.send(()).unwrap();
    handle.join().unwrap();
}
