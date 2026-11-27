use rusqlite::Connection;
use sha2::{Digest, Sha256};
use store::plugins::{
    ApplyPluginPackageSwitch, GrantPluginUiTrust, PluginError, PluginLifecycleObservation,
    PluginStatePolicy, PluginVersionMetadata,
};
use store::{Store, StoreError};

fn app_hash(dir: &std::path::Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(dir.join("app.db")).unwrap()).to_vec()
}

fn manifest_with_ui(plugin_id: &str, remote_name: &str) -> String {
    serde_json::json!({
        "manifest_version": 1,
        "id": plugin_id,
        "name": "UI Plugin",
        "version": "1.0.0",
        "api": "1.0.0",
        "license": "MIT",
        "authors": ["Test"],
        "description": "UI test plugin",
        "config_schema": "{}",
        "state_schema": 1,
        "ui": {
            "kind": "federation-react",
            "contract": "1.0.0",
            "react": "19.3.0",
            "react_dom": "19.3.0",
            "remote_name": remote_name,
            "entry": "ui/assets/remoteEntry.js",
            "asset_manifest": "ui/asset-manifest.json",
            "capabilities": ["read:sites"],
            "surfaces": [{
                "kind": "page",
                "id": "main",
                "expose": "./Page",
                "title": "Main",
                "icon": "puzzle"
            }]
        }
    })
    .to_string()
}

fn ver_meta(
    plugin_id: &str,
    package_hash: &str,
    version: &str,
    remote_name: &str,
    sig_status: &str,
) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: plugin_id.into(),
        package_hash: package_hash.into(),
        version: version.into(),
        manifest_json: manifest_with_ui(plugin_id, remote_name),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: String::new(),
        signing_key_id: None,
        signature_status: sig_status.into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

#[test]
fn ui_trust_schema_validation_rejects_missing_or_tampered_table() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    conn.execute_batch("DROP TABLE plugin_ui_trust").unwrap();
    drop(conn);
    let before = app_hash(dir.path());
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(app_hash(dir.path()), before);
}

#[test]
fn ui_trust_grant_revoke_regrant_and_per_hash_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pid = "org.example.uitrust";

    assert_eq!(store.get_ui_trust(pid, "pkg-1").unwrap(), None);
    assert_eq!(store.get_active_ui_trust(pid, "pkg-1").unwrap(), None);

    // Wrong initial expected revision (1 instead of 0) fails with Conflict.
    let err = store
        .grant_ui_trust(&GrantPluginUiTrust {
            plugin_id: pid.into(),
            package_sha256: "pkg-1".into(),
            bundle_sha256: "bundle-1".into(),
            granted_by_user_id: "admin-1".into(),
            granted_at: "2026-11-23T00:00:00Z".into(),
            expected_revision: 1,
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::Plugin(PluginError::Conflict)));

    // Initial grant succeeds with revision=1.
    let g1 = store
        .grant_ui_trust(&GrantPluginUiTrust {
            plugin_id: pid.into(),
            package_sha256: "pkg-1".into(),
            bundle_sha256: "bundle-1".into(),
            granted_by_user_id: "admin-1".into(),
            granted_at: "2026-11-23T00:00:00Z".into(),
            expected_revision: 0,
        })
        .unwrap();
    assert_eq!(g1.revision, 1);
    assert_eq!(g1.revoked_at, None);
    assert_eq!(g1.bundle_sha256, "bundle-1");

    // Per-hash isolation: pkg-2 is not trusted.
    assert_eq!(
        store.get_active_ui_trust(pid, "pkg-1").unwrap(),
        Some(g1.clone())
    );
    assert_eq!(store.get_active_ui_trust(pid, "pkg-2").unwrap(), None);

    // Stale CAS revoke fails.
    let err_rev = store
        .revoke_ui_trust(pid, "pkg-1", 99, "2026-11-23T00:01:00Z")
        .unwrap_err();
    assert!(matches!(err_rev, StoreError::Plugin(PluginError::Conflict)));

    // Revoke with matching revision succeeds.
    let r1 = store
        .revoke_ui_trust(pid, "pkg-1", 1, "2026-11-23T00:01:00Z")
        .unwrap();
    assert_eq!(r1.revision, 2);
    assert_eq!(r1.revoked_at.as_deref(), Some("2026-11-23T00:01:00Z"));
    assert_eq!(store.get_active_ui_trust(pid, "pkg-1").unwrap(), None);
    assert_eq!(store.get_ui_trust(pid, "pkg-1").unwrap(), Some(r1));

    // Re-grant with revision=2 restores active trust and bumps revision to 3.
    let g2 = store
        .grant_ui_trust(&GrantPluginUiTrust {
            plugin_id: pid.into(),
            package_sha256: "pkg-1".into(),
            bundle_sha256: "bundle-1".into(),
            granted_by_user_id: "admin-2".into(),
            granted_at: "2026-11-23T00:02:00Z".into(),
            expected_revision: 2,
        })
        .unwrap();
    assert_eq!(g2.revision, 3);
    assert_eq!(g2.revoked_at, None);
    assert_eq!(g2.granted_by_user_id, "admin-2");
    assert_eq!(store.get_active_ui_trust(pid, "pkg-1").unwrap(), Some(g2));

    // Re-open keeps durable trust state.
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    let persisted = reopened.get_active_ui_trust(pid, "pkg-1").unwrap().unwrap();
    assert_eq!(persisted.revision, 3);
}

#[test]
fn remote_name_uniqueness_enforced_across_plugins_and_cleaned_on_uninstall_or_dev_reload() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let v1 = ver_meta(
        "org.example.r1",
        "hash-r1",
        "1.0.0",
        "sharedRemote",
        "development",
    );
    let _rec1 = store.register_plugin_version(&v1, 0).unwrap();

    // Another plugin registering the same ui.remote_name fails.
    let dup = ver_meta(
        "org.example.r2",
        "hash-r2",
        "1.0.0",
        "sharedRemote",
        "signed",
    );
    let err = store.register_plugin_version(&dup, 0).unwrap_err();
    assert!(matches!(
        err,
        StoreError::Plugin(PluginError::Invalid(_) | PluginError::Conflict)
    ));
    assert_eq!(
        store
            .find_conflicting_remote_name_plugin("org.example.r2", "sharedRemote")
            .unwrap()
            .as_deref(),
        Some("org.example.r1")
    );
    assert_eq!(
        store
            .find_conflicting_remote_name_plugin("org.example.r1", "sharedRemote")
            .unwrap(),
        None
    );

    // Distinct remote_name succeeds.
    let distinct = ver_meta("org.example.r2", "hash-r2", "1.0.0", "remoteTwo", "signed");
    let rec2 = store.register_plugin_version(&distinct, 0).unwrap();

    // Upgrading r2 to clash with r1's remote_name fails at version registration AND on switch.
    let r2_clash = ver_meta(
        "org.example.r2",
        "hash-r2-v2",
        "1.1.0",
        "sharedRemote",
        "signed",
    );
    assert!(matches!(
        store.register_plugin_version(&r2_clash, rec2.revision),
        Err(StoreError::Plugin(
            PluginError::Invalid(_) | PluginError::Conflict
        ))
    ));
    let r2_ok = ver_meta(
        "org.example.r2",
        "hash-r2-v2-ok",
        "1.1.0",
        "remoteSwap",
        "signed",
    );
    let rec2 = store
        .register_plugin_version(&r2_ok, rec2.revision)
        .unwrap();
    let r4_claims = ver_meta("org.example.r4", "hash-r4", "1.0.0", "remoteSwap", "signed");
    store.register_plugin_version(&r4_claims, 0).unwrap();
    let switch_err = store
        .apply_plugin_package_switch(&ApplyPluginPackageSwitch {
            plugin_id: "org.example.r2".into(),
            expected_revision: rec2.revision,
            operation_id: None,
            target_package_hash: "hash-r2-v2-ok".into(),
            state_policy: PluginStatePolicy::Preserve,
            target_config: None,
            target_grants: vec![],
        })
        .unwrap_err();
    assert!(matches!(
        switch_err,
        StoreError::Plugin(PluginError::Invalid(_) | PluginError::Conflict)
    ));
}

#[test]
fn ui_trust_cleaned_on_dev_reload_and_uninstall_freeing_remote_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let v1 = ver_meta(
        "org.example.r1",
        "hash-r1",
        "1.0.0",
        "sharedRemote",
        "development",
    );
    store.register_plugin_version(&v1, 0).unwrap();
    let distinct = ver_meta("org.example.r2", "hash-r2", "1.0.0", "remoteTwo", "signed");
    store.register_plugin_version(&distinct, 0).unwrap();

    // Grant trust on r1, then dev-reload to hash-r1-v2 -> old trust row removed.
    store
        .grant_ui_trust(&GrantPluginUiTrust {
            plugin_id: "org.example.r1".into(),
            package_sha256: "hash-r1".into(),
            bundle_sha256: "b1".into(),
            granted_by_user_id: "admin".into(),
            granted_at: "2026-11-23T00:00:00Z".into(),
            expected_revision: 0,
        })
        .unwrap();
    let v1_dev = ver_meta(
        "org.example.r1",
        "hash-r1-v2",
        "1.0.1",
        "sharedRemote",
        "development",
    );
    let rec1 = store.replace_dev_plugin_version(&v1_dev, &[]).unwrap();
    assert_eq!(
        store.get_ui_trust("org.example.r1", "hash-r1").unwrap(),
        None
    );

    // Dev reload clashing with remoteTwo must also fail.
    let v1_clash = ver_meta(
        "org.example.r1",
        "hash-r1-v3",
        "1.0.2",
        "remoteTwo",
        "development",
    );
    let dev_err = store
        .replace_dev_plugin_version(&v1_clash, &[])
        .unwrap_err();
    assert!(matches!(
        dev_err,
        StoreError::Plugin(PluginError::Invalid(_) | PluginError::Conflict)
    ));

    // Grant trust on hash-r1-v2 then uninstall r1 -> removes plugin_ui_trust & frees sharedRemote.
    store
        .grant_ui_trust(&GrantPluginUiTrust {
            plugin_id: "org.example.r1".into(),
            package_sha256: "hash-r1-v2".into(),
            bundle_sha256: "b2".into(),
            granted_by_user_id: "admin".into(),
            granted_at: "2026-11-23T00:05:00Z".into(),
            expected_revision: 0,
        })
        .unwrap();
    store
        .uninstall_plugin("org.example.r1", rec1.revision, None, true)
        .unwrap();
    assert_eq!(
        store.get_ui_trust("org.example.r1", "hash-r1-v2").unwrap(),
        None
    );

    // Now another plugin can claim sharedRemote cleanly.
    let _ = PluginLifecycleObservation {
        plugin_id: String::new(),
        revision: 0,
        generation: 0,
        operation_id: uuid::Uuid::nil(),
        workers_exited: true,
        unknown_effects_reconciled: true,
        validation_succeeded: true,
    };
    let reused = ver_meta(
        "org.example.r3",
        "hash-r3",
        "1.0.0",
        "sharedRemote",
        "signed",
    );
    store.register_plugin_version(&reused, 0).unwrap();
}
