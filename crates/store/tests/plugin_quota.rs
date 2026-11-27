#[path = "plugin_support/mod.rs"]
mod plugin_support;
use plugin_support::*;
use std::sync::{Arc, Barrier};
use store::{Store, StoreError, plugins::*};

fn total(store: &Store, a: &PluginStateAuthority, kind: PluginStateKind) -> i64 {
    let quota = store.plugin_state_quota(a, kind).unwrap();
    match kind {
        PluginStateKind::Kv => quota.plugin_kv_bytes,
        PluginStateKind::Blob => quota.plugin_blob_bytes,
    }
}
fn fill(store: &Store, a: &PluginStateAuthority, kind: PluginStateKind, limit: i64) {
    let (count, size) = match kind {
        PluginStateKind::Kv => (79, 65536),
        PluginStateKind::Blob => (49, 1048576),
    };
    let bytes = vec![5; size];
    for i in 0..count {
        store
            .plugin_state_put(a, kind, &format!("fill{i:02}"), &bytes, 0)
            .unwrap();
    }
    let tail = vec![4; (limit - 10 - total(store, a, kind) - 4) as usize];
    store.plugin_state_put(a, kind, "tail", &tail, 0).unwrap();
    assert_eq!(total(store, a, kind), limit - 10);
}

#[test]
fn two_scopes_contend_for_aggregate_quota_and_empty_values_cost_key_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let p = enable(
        &store,
        &install(&store, "org.example.quota", &[a.clone(), b.clone()]),
    );
    let aa = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    let bb = store
        .issue_plugin_state_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    for (kind, limit) in [
        (PluginStateKind::Kv, 5 * 1024 * 1024),
        (PluginStateKind::Blob, 50 * 1024 * 1024),
    ] {
        fill(&store, &aa, kind, limit);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = [(aa.clone(), "a"), (bb.clone(), "b")]
            .into_iter()
            .map(|(a, key)| {
                let (store, barrier) = (store.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let result = store.plugin_state_put(&a, kind, key, &[8; 8], 0);
                    (a, key, result)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|(_, _, r)| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|(_, _, r)| matches!(
                    r,
                    Err(StoreError::Plugin(PluginError::QuotaExceeded))
                ))
                .count(),
            1
        );
        assert_eq!(total(&store, &aa, kind), limit - 1);
        let (authority, key, winner) = results.into_iter().find(|(_, _, r)| r.is_ok()).unwrap();
        let winner = winner.unwrap();
        let empty = store
            .plugin_state_put(&authority, kind, "c", b"", 0)
            .unwrap();
        assert_eq!(total(&store, &aa, kind), limit);
        assert!(matches!(
            store.plugin_state_put(&authority, kind, "d", b"", 0),
            Err(StoreError::Plugin(PluginError::QuotaExceeded))
        ));
        assert_eq!(store.plugin_state_get(&authority, kind, "d").unwrap(), None);
        store
            .plugin_state_delete(&authority, kind, "c", empty.revision)
            .unwrap();
        let replacement = store
            .plugin_state_put(&authority, kind, key, b"", winner.revision)
            .unwrap();
        assert_eq!(total(&store, &aa, kind), limit - 9);
        assert!(matches!(
            store.plugin_state_put(&authority, kind, key, b"old", winner.revision),
            Err(StoreError::Plugin(PluginError::Conflict))
        ));
        store
            .plugin_state_delete(&authority, kind, key, replacement.revision)
            .unwrap();
        assert_eq!(total(&store, &aa, kind), limit - 10);
        let recreated = store
            .plugin_state_put(&authority, kind, key, b"", 0)
            .unwrap();
        assert!(recreated.revision > replacement.revision);
        assert!(
            store
                .plugin_state_delete(&authority, kind, key, winner.revision)
                .is_err()
        );
        store
            .plugin_state_delete(&authority, kind, key, recreated.revision)
            .unwrap();
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(total(&reopened, &aa, kind), limit - 10);
    }
}

#[test]
fn object_key_and_etag_limits_reject_without_partial_changes_and_reads_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = enable(&store, &install(&store, "org.example.limits", &[]));
    let a = store.issue_plugin_state_authority(&p, None).unwrap();
    for (kind, max) in [
        (PluginStateKind::Kv, 65536),
        (PluginStateKind::Blob, 1048576),
    ] {
        let before = store.plugin_state_quota(&a, kind).unwrap();
        assert!(
            store
                .plugin_state_put(&a, kind, "too-large", &vec![0; max + 1], 0)
                .is_err()
        );
        assert!(
            store
                .plugin_state_put(&a, kind, &"界".repeat(86), b"", 0)
                .is_err()
        );
        assert!(store.plugin_state_put(&a, kind, "", b"", 0).is_err());
        assert_eq!(store.plugin_state_quota(&a, kind).unwrap(), before);
        let key = format!("{}x", "界".repeat(85));
        let entry = store
            .plugin_state_put(&a, kind, &key, &vec![0; max], 0)
            .unwrap();
        assert_eq!(total(&store, &a, kind), (max + 256) as i64);
        store
            .plugin_state_delete(&a, kind, &key, entry.revision)
            .unwrap();
        assert_eq!(total(&store, &a, kind), 0);
    }
    assert!(
        store
            .app_write(|app| {
                app.plugin_state_put(&a, PluginStateKind::Kv, "rollback", b"123", 0)?;
                Err::<(), _>(StoreError::Invalid("abort fixture".into()))
            })
            .is_err()
    );
    assert_eq!(
        store
            .plugin_state_get(&a, PluginStateKind::Kv, "rollback")
            .unwrap(),
        None
    );
    assert_eq!(total(&store, &a, PluginStateKind::Kv), 0);
    for i in 0..205 {
        store
            .plugin_state_put(&a, PluginStateKind::Kv, &format!("key{i:03}"), b"", 0)
            .unwrap();
    }
    assert_eq!(
        store
            .plugin_state_list(&a, PluginStateKind::Kv, None, None)
            .unwrap()
            .len(),
        100
    );
    let first = store
        .plugin_state_list(&a, PluginStateKind::Kv, None, Some(999))
        .unwrap();
    assert_eq!(first.len(), 200);
    let rest = store
        .plugin_state_list(
            &a,
            PluginStateKind::Kv,
            Some(&first.last().unwrap().key),
            Some(200),
        )
        .unwrap();
    assert_eq!(rest.len(), 5);
    assert!(rest.first().unwrap().key > first.last().unwrap().key);
}

#[test]
fn site_deletion_and_credential_epoch_rotation_purge_retired_quota_state_and_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, b) = (site(), site());
    store.insert_site(&a).unwrap();
    store.insert_site(&b).unwrap();
    let p_disabled = install(&store, "org.example.retired-scope", &[a.clone(), b.clone()]);
    let cfg_a = store
        .issue_plugin_config_authority(&p_disabled, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    let p_configured = store
        .save_plugin_config_with_secrets(
            &cfg_a,
            p_disabled.revision,
            "1",
            &serde_json::json!({}),
            &[PluginSecretUpdate {
                key: "password".into(),
                change: PluginSecretChange::Replace(b"sec-a".to_vec()),
            }],
        )
        .unwrap();
    let p = enable(&store, &p_configured);

    // 1. Exhaust 5 MiB KV quota on site `a` (epoch 1), then rotate `a`'s credentials.
    let kv_limit = 5 * 1024 * 1024;
    let aa1 = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    fill(&store, &aa1, PluginStateKind::Kv, kv_limit);
    store
        .plugin_state_put(&aa1, PluginStateKind::Kv, "cap1", &[7; 6], 0)
        .unwrap();
    assert_eq!(total(&store, &aa1, PluginStateKind::Kv), kv_limit);

    store
        .save_site(&a, store::ManualCredentialWrite::Replace)
        .unwrap();

    let summary_after_rotate = store.get_plugin_config_summary(&p.plugin_id).unwrap();
    assert!(
        summary_after_rotate.secrets.is_empty(),
        "retired epoch secrets must be purged on credential rotation: {:?}",
        summary_after_rotate.secrets
    );
    let aa2 = store
        .issue_plugin_state_authority(&p, store.observe_site(a.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(total(&store, &aa2, PluginStateKind::Kv), 0);

    // 2. Fill 5 MiB KV quota on `a` (epoch 2), then delete site `a` -> site `b` must be able to write!
    fill(&store, &aa2, PluginStateKind::Kv, kv_limit);
    store
        .plugin_state_put(&aa2, PluginStateKind::Kv, "cap2", &[7; 6], 0)
        .unwrap();
    assert!(store.delete_site(a.id).unwrap());

    let bb = store
        .issue_plugin_state_authority(&p, store.observe_site(b.id).unwrap().as_ref())
        .unwrap();
    assert_eq!(total(&store, &bb, PluginStateKind::Kv), 0);
    store
        .plugin_state_put(&bb, PluginStateKind::Kv, "live-b", b"ok", 0)
        .expect("live site b must not hit QuotaExceeded after site a is deleted");
}
