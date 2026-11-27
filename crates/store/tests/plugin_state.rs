#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use store::{ManualCredentialWrite, Store, StoreError, plugins::*};

#[test]
fn credential_state_isolated_and_survives_refresh_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let p = enable(
        &store,
        &install(&store, "org.example.bound", &[a.clone(), b.clone()]),
    );
    let observed = store.observe_site(a.id).unwrap().unwrap();
    let aa = store
        .issue_plugin_state_authority(&p, Some(&observed))
        .unwrap();
    let bb = store
        .issue_plugin_state_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    assert!(matches!(
        store.issue_plugin_state_authority(&p, None),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    let global = enable(&store, &install(&store, "org.example.global", &[]));
    let global = store.issue_plugin_state_authority(&global, None).unwrap();
    for kind in [PluginStateKind::Kv, PluginStateKind::Blob] {
        let value = store
            .plugin_state_put(&aa, kind, "token", b"domain-a-only", 0)
            .unwrap();
        assert_eq!(value.value.security_scope(), aa.security_scope());
        assert_eq!(store.plugin_state_get(&bb, kind, "token").unwrap(), None);
        assert_eq!(
            store.plugin_state_get(&global, kind, "token").unwrap(),
            None
        );
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(
            reopened.plugin_state_get(&aa, kind, "token").unwrap(),
            Some(value)
        );
        assert_eq!(reopened.plugin_state_get(&bb, kind, "token").unwrap(), None);
    }
    assert!(
        store
            .save_site_if_current(&observed, Some("verified-refresh"))
            .unwrap()
    );
    let refreshed = store.observe_site(a.id).unwrap().unwrap();
    let refreshed_authority = store
        .issue_plugin_state_authority(&p, Some(&refreshed))
        .unwrap();
    assert_eq!(refreshed_authority.security_scope(), aa.security_scope());
    assert!(
        store
            .plugin_state_get(&aa, PluginStateKind::Kv, "token")
            .unwrap()
            .is_some()
    );
    assert!(
        !store
            .save_site_if_current(&observed, Some("stale-refresh"))
            .unwrap()
    );
}

#[test]
fn admin_config_secrets_are_write_only_and_scope_bound() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let mut p = install(&store, "org.example.bound", &[a.clone(), b.clone()]);
    let observation = store.observe_site(a.id).unwrap().unwrap();
    let old = store
        .issue_plugin_config_authority(&p, Some(&observation))
        .unwrap();
    p = store
        .save_plugin_config_with_secrets(
            &old,
            p.revision,
            "1",
            &serde_json::json!({"visible":true}),
            &[PluginSecretUpdate {
                key: "password".into(),
                change: PluginSecretChange::Replace(b"private-domain-a".to_vec()),
            }],
        )
        .unwrap();
    assert!(matches!(
        store.export_plugin_config_secret(&old, "password"),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    let aa = store
        .issue_plugin_config_authority(&p, Some(&observation))
        .unwrap();
    let bb = store
        .issue_plugin_config_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    assert!(
        store
            .plugin_config_secret_configured(&aa, "password")
            .unwrap()
    );
    assert!(
        !store
            .plugin_config_secret_configured(&bb, "password")
            .unwrap()
    );
    assert_eq!(
        store.export_plugin_config_secret(&bb, "password").unwrap(),
        None
    );
    assert_eq!(
        Store::open(dir.path())
            .unwrap()
            .export_plugin_config_secret(&aa, "password")
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"private-domain-a"
    );
    assert_eq!(
        Store::open(dir.path())
            .unwrap()
            .export_plugin_config_secret(&bb, "password")
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .get_plugin_config(&p.plugin_id)
            .unwrap()
            .unwrap()
            .values,
        serde_json::json!({"visible":true})
    );
    let before = p.clone();
    assert!(
        store
            .app_write(|app| {
                app.save_plugin_config_with_secrets(
                    &aa,
                    p.revision,
                    "2",
                    &serde_json::json!({}),
                    &[PluginSecretUpdate {
                        key: "password".into(),
                        change: PluginSecretChange::Clear,
                    }],
                )?;
                Err::<(), _>(StoreError::Invalid("abort fixture".into()))
            })
            .is_err()
    );
    assert_eq!(store.get_plugin(&p.plugin_id).unwrap(), Some(before));
    assert!(
        store
            .plugin_config_secret_configured(&aa, "password")
            .unwrap()
    );
    let mut changed = a.clone();
    changed.url = "https://replacement.invalid/".into();
    store
        .save_site(&changed, ManualCredentialWrite::Unchanged)
        .unwrap();
    assert!(store.export_plugin_config_secret(&aa, "password").is_err());
    assert!(
        store
            .issue_plugin_config_authority(&p, Some(&observation))
            .is_err()
    );
    let new = store
        .issue_plugin_config_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(
        store.export_plugin_config_secret(&new, "password").unwrap(),
        None
    );
}

#[test]
fn replacement_recreation_generation_and_revocation_invalidate_authority() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = site();
    store.insert_site(&a).unwrap();
    let mut p = enable(
        &store,
        &install(&store, "org.example.bound", std::slice::from_ref(&a)),
    );
    let captured = store.observe_site(a.id).unwrap().unwrap();
    let old = store
        .issue_plugin_state_authority(&p, Some(&captured))
        .unwrap();
    for kind in [PluginStateKind::Kv, PluginStateKind::Blob] {
        store
            .plugin_state_put(&old, kind, "token", b"secret", 0)
            .unwrap();
    }
    store.save_site(&a, ManualCredentialWrite::Replace).unwrap();
    assert!(
        store
            .issue_plugin_state_authority(&p, Some(&captured))
            .is_err()
    );
    for kind in [PluginStateKind::Kv, PluginStateKind::Blob] {
        assert!(matches!(
            store.plugin_state_get(&old, kind, "token"),
            Err(StoreError::Plugin(PluginError::StaleAuthority))
        ));
    }
    let newer = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(
        store
            .plugin_state_get(&newer, PluginStateKind::Kv, "token")
            .unwrap(),
        None
    );
    store.delete_site(a.id).unwrap();
    store.insert_site(&a).unwrap();
    assert!(
        store
            .plugin_state_put(&newer, PluginStateKind::Kv, "token", b"stale", 0)
            .is_err()
    );
    let recreated = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(
        store
            .plugin_state_get(&recreated, PluginStateKind::Kv, "token")
            .unwrap(),
        None
    );
    p = disable(&store, &p);
    assert!(
        store
            .plugin_state_get(&recreated, PluginStateKind::Kv, "token")
            .is_err()
    );
    p = store
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &[])
        .unwrap();
    p = enable(&store, &p);
    let denied = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert!(matches!(
        store.plugin_state_get(&denied, PluginStateKind::Kv, "token"),
        Err(StoreError::Plugin(PluginError::Denied))
    ));
    assert!(
        store
            .plugin_state_get(&recreated, PluginStateKind::Kv, "token")
            .is_err()
    );
}

#[test]
fn prefix_entry_listing_respects_domain_cursor_and_blob_cap() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.prefix", &[]));
    let auth = store.issue_plugin_state_authority(&p, None).unwrap();

    for key in ["item/a", "item/b", "item/c", "other/x", "item%literal/z"] {
        store
            .plugin_state_put(&auth, PluginStateKind::Kv, key, key.as_bytes(), 0)
            .unwrap();
    }
    let page1 = store
        .plugin_state_list_entries(&auth, PluginStateKind::Kv, "item/", None, Some(2))
        .unwrap();
    let keys1: Vec<_> = page1.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys1, vec!["item/a", "item/b"]);
    let page2 = store
        .plugin_state_list_entries(
            &auth,
            PluginStateKind::Kv,
            "item/",
            Some("item/b"),
            Some(10),
        )
        .unwrap();
    let keys2: Vec<_> = page2.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys2, vec!["item/c"]);
    assert_eq!(page2[0].value.as_bytes(), b"item/c");

    // Prefix matching treats SQL wildcards literally and clamps blob pages to 32 items.
    let wild = store
        .plugin_state_list_entries(&auth, PluginStateKind::Kv, "item%literal/", None, None)
        .unwrap();
    assert_eq!(wild.len(), 1);
    assert_eq!(wild[0].key, "item%literal/z");

    for idx in 0..40 {
        store
            .plugin_state_put(
                &auth,
                PluginStateKind::Blob,
                &format!("b/{idx:02}"),
                b"payload",
                0,
            )
            .unwrap();
    }
    let blobs = store
        .plugin_state_list_entries(&auth, PluginStateKind::Blob, "b/", None, Some(200))
        .unwrap();
    assert_eq!(blobs.len(), 32);
    assert!(
        store
            .plugin_state_list_entries(&auth, PluginStateKind::Kv, &"x".repeat(257), None, None)
            .is_err()
    );
}
