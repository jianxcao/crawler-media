use domain::{DownloaderId, SiteId};
use store::download_operations::{
    AcceptDownloadOperationInput, CommitBoostFinalInput, ReserveBoostProvisionalInput,
};
use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause,
    PluginCommandOutcome, PluginEffectObservation, PluginError, PluginGrant, PluginGrantScope,
    PluginLifecycleObservation, PluginOperationState, PluginSubject, PluginVersionMetadata,
    StagedPluginIntent, StagedPluginOpKind,
};
use store::{Store, StoreError};

fn dev_version(id: &str, ver: &str, hash: &str) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: hash.into(),
        version: ver.into(),
        manifest_json: "{}".into(),
        config_schema_json: r#"{"type":"object","additionalProperties":false,"properties":{}}"#
            .into(),
        state_schema: "1".into(),
        artifact_path: format!("plugins/{id}/{hash}.cmplugin"),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    }
}

#[test]
fn deferred_command_rejects_negative_next_run_at() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    let pid = "org.example.r8-neg-defer";
    let p = store
        .register_plugin_version(&dev_version(pid, "1.0.0", "h1"), 0)
        .unwrap();
    let acc = store
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.into()),
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "r8-neg-1".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/commands".into(),
                body_digest: [1; 32],
            },
            kind: "site.check-in".into(),
            expected_revision: Some(p.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("r8-neg"),
        })
        .unwrap();
    let claim = store
        .claim_plugin_command(
            acc.operation.operation_id,
            acc.operation.revision,
            None,
            0,
            10,
        )
        .unwrap();

    let err = store
        .record_plugin_command_outcome(
            &claim,
            &PluginCommandOutcome {
                state: PluginOperationState::Deferred,
                phase: "retry-wait".into(),
                next_run_at: Some(-2),
                result: None,
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .expect_err("negative next_run_at must be rejected");
    assert!(matches!(err, StoreError::Plugin(PluginError::Invalid(_))));
}

#[test]
fn boost_final_reservation_rejects_negative_now_utc_without_underflow() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();

    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-r8-uf".into(),
            idempotency_key: "ik-r8-uf".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            cause_id: "cause:1".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/uf.torrent".into(),
            ownership_tag: "cm-op:op-r8-uf".into(),
            info_hash: Some("abcd0001".into()),
            bytes_digest: Some("dig-uf".into()),
            verified_bytes: Some(1024),
            request_digest: "req-uf".into(),
            now_utc: 100,
        })
        .unwrap();
    let prov = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-r8-uf".into(),
            operation_id: "op-r8-uf".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: Some(site_id),
            downloader_id: dl_id,
            info_hash: Some("abcd0001".into()),
            estimated_bytes: 1024,
            expires_at: Some(500),
            now_utc: 100,
        })
        .unwrap();

    let err = store
        .commit_boost_final_reservation(&CommitBoostFinalInput {
            operation_id: "op-r8-uf".into(),
            expected_reservation_revision: prov.revision,
            info_hash: "abcd0001".into(),
            bytes_digest: "dig-uf".into(),
            verified_bytes: 1024,
            budget_bytes: 1_000_000,
            max_add_per_hour: Some(5),
            now_utc: i64::MIN,
            mark_operation_submitting: false,
        })
        .expect_err("i64::MIN / negative now_utc must fail cleanly without underflow");
    assert!(matches!(err, StoreError::Plugin(PluginError::Invalid(_))));
}

#[test]
fn uninstall_and_reinstall_preserves_monotonic_generation_floor() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    let pid = "org.example.r8-gen-floor";
    let p1 = store
        .register_plugin_version(&dev_version(pid, "1.0.0", "h1"), 0)
        .unwrap();
    assert_eq!(p1.generation, 0);

    let op_en = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(pid, p1.revision, op_en)
        .unwrap();
    let en = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: pid.into(),
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
    let op_dis = uuid::Uuid::new_v4();
    let dr = store
        .begin_plugin_lifecycle(pid, en.revision, op_dis, false)
        .unwrap();
    let dis = store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: pid.into(),
                operation_id: op_dis,
                revision: dr.revision,
                generation: dr.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .unwrap();
    let prior_gen = dis.generation;
    store
        .uninstall_plugin(pid, dis.revision, None, true)
        .unwrap();

    // Direct reinstall AND staged reinstall must both start strictly above `prior_gen`.
    let p2 = store
        .register_plugin_version(&dev_version(pid, "1.0.1", "h2"), 0)
        .unwrap();
    assert!(
        p2.generation > prior_gen,
        "reinstalled generation {} must exceed uninstalled generation {}",
        p2.generation,
        prior_gen
    );
    store
        .uninstall_plugin(pid, p2.revision, None, true)
        .unwrap();

    let begin = store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "r8-st-inst".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/install".into(),
                body_digest: [5; 32],
            },
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            cause: PluginCommandCause::root("r8-st"),
            intent: StagedPluginIntent {
                target_plugin_id: pid.into(),
                package_hash: Some("h3".into()),
                version: Some("1.0.2".into()),
                artifact_path: None,
                state_policy: None,
                target_config_digest: None,
                grants: vec![PluginGrant {
                    capability: "kv".into(),
                    scope: PluginGrantScope::Global,
                    sensitive: false,
                }],
            },
        })
        .unwrap();
    let mut claim = store
        .claim_staged_plugin_operation(
            begin.operation.operation_id,
            begin.operation.revision,
            0,
            20,
        )
        .unwrap();
    let p3 = store
        .app_write(|app| {
            app.register_staged_plugin_version(
                &mut claim,
                &dev_version(pid, "1.0.2", "h3"),
                &[],
                20,
            )
        })
        .unwrap();
    assert!(
        p3.generation > p2.generation,
        "staged reinstall generation {} must exceed {}",
        p3.generation,
        p2.generation
    );
}
