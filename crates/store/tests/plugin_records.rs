use store::{Store, StoreError, plugins::*};

fn metadata(hash: &str, version: &str) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: "org.example.storage".into(),
        package_hash: hash.into(),
        version: version.into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "/not-read-by-store/package".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

#[test]
fn metadata_registration_reopens_disabled_and_preserves_immutable_versions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = metadata("digest-a", "1.0.0");
    let p = store.register_plugin_version(&first, 0).unwrap();
    assert_eq!(p.lifecycle, PluginLifecycle::InstalledDisabled);
    assert!(!p.desired_enabled);
    assert_eq!((p.revision, p.config_revision, p.generation), (1, 0, 0));
    assert!(matches!(
        store.register_plugin_version(&metadata("digest-b", "1.0.0"), p.revision),
        Err(StoreError::Plugin(PluginError::PackageConflict))
    ));
    let second = metadata("digest-b", "2.0.0");
    store.register_plugin_version(&second, p.revision).unwrap();
    assert!(matches!(
        store.register_plugin_version(&metadata("digest-b", "3.0.0"), p.revision),
        Err(StoreError::Plugin(PluginError::PackageConflict))
    ));
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_plugin(&first.plugin_id).unwrap(), Some(p));
    assert_eq!(
        reopened
            .get_plugin_version(&first.plugin_id, "digest-a")
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        reopened
            .get_plugin_version(&second.plugin_id, "digest-b")
            .unwrap(),
        Some(second)
    );
}

#[test]
fn lifecycle_requires_fenced_host_evidence_and_config_requires_disabled_drained() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut p = store
        .register_plugin_version(&metadata("a", "1"), 0)
        .unwrap();
    let id = p.plugin_id.clone();
    p = store
        .save_plugin_config(
            &id,
            p.revision,
            "schema-1",
            &serde_json::json!({"public":true}),
        )
        .unwrap();
    assert_eq!(p.config_revision, 1);
    assert!(matches!(
        store.save_plugin_config(&id, 1, "x", &serde_json::json!({})),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    let op = uuid::Uuid::new_v4();
    p = store
        .begin_plugin_lifecycle(&id, p.revision, op, true)
        .unwrap();
    let mut evidence = PluginLifecycleObservation {
        plugin_id: id.clone(),
        operation_id: op,
        revision: p.revision,
        generation: p.generation,
        workers_exited: false,
        unknown_effects_reconciled: false,
        validation_succeeded: true,
    };
    assert!(matches!(
        store.finalize_plugin_lifecycle(&evidence),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    evidence.workers_exited = true;
    evidence.unknown_effects_reconciled = true;
    p = store.finalize_plugin_lifecycle(&evidence).unwrap();
    assert_eq!(p.lifecycle, PluginLifecycle::Enabled);
    assert!(matches!(
        store.save_plugin_config(&id, p.revision, "x", &serde_json::json!({})),
        Err(StoreError::Plugin(PluginError::NotDrained))
    ));
    let old_generation = p.generation;
    p = store
        .begin_plugin_lifecycle(&id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    assert!(p.generation > old_generation);
    assert_eq!(p.lifecycle, PluginLifecycle::Draining);
    assert!(
        store
            .save_plugin_config(&id, p.revision, "x", &serde_json::json!({}))
            .is_err()
    );
    assert!(matches!(
        store.finalize_plugin_lifecycle(&evidence),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_plugin(&id).unwrap(), Some(p.clone()));
    evidence.operation_id = p.active_operation_id.unwrap();
    evidence.revision = p.revision;
    evidence.generation = p.generation;
    p = reopened.finalize_plugin_lifecycle(&evidence).unwrap();
    assert_eq!(p.lifecycle, PluginLifecycle::Disabled);
    let before = p.revision;
    let grants = [PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    p = reopened
        .replace_plugin_grants(&id, "a", before, &grants)
        .unwrap();
    assert!(p.revision > before);
    assert_eq!(reopened.get_plugin_grants(&id).unwrap(), grants);
    assert_eq!(
        reopened.get_plugin_config(&id).unwrap().unwrap().values,
        serde_json::json!({"public":true})
    );
}

#[test]
fn lifecycle_begin_persists_intent_and_fences_finalization() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = store
        .register_plugin_version(&metadata("a", "1"), 0)
        .unwrap();
    let op = uuid::Uuid::new_v4();
    let intent = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, op, true)
        .unwrap();
    assert_eq!(intent.lifecycle, PluginLifecycle::Enabling);
    assert_eq!(intent.active_operation_id, Some(op));
    assert!(intent.revision > p.revision && intent.generation > p.generation);
    assert_eq!(
        Store::open(dir.path())
            .unwrap()
            .get_plugin(&p.plugin_id)
            .unwrap(),
        Some(intent.clone())
    );
    let evidence = PluginLifecycleObservation {
        plugin_id: p.plugin_id.clone(),
        operation_id: op,
        revision: intent.revision,
        generation: p.generation,
        workers_exited: true,
        unknown_effects_reconciled: true,
        validation_succeeded: true,
    };
    assert!(matches!(
        store.finalize_plugin_lifecycle(&evidence),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
}

#[test]
fn grant_replacement_is_digest_bound_cas() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let p = store
        .register_plugin_version(&metadata("a", "1"), 0)
        .unwrap();
    let grants = [PluginGrant {
        capability: "kv".into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    }];
    let updated = store
        .replace_plugin_grants(&p.plugin_id, "a", p.revision, &grants)
        .unwrap();
    assert!(updated.generation > p.generation);
    assert_eq!(store.get_plugin_grants(&p.plugin_id).unwrap(), grants);
    assert!(matches!(
        store.replace_plugin_grants(&p.plugin_id, "b", updated.revision, &[]),
        Err(StoreError::Plugin(PluginError::StaleAuthority))
    ));
    assert!(matches!(
        store.replace_plugin_grants(&p.plugin_id, "a", p.revision, &[]),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));
    assert_eq!(store.get_plugin_grants(&p.plugin_id).unwrap(), grants);
}
