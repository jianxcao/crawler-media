use domain::{DownloaderId, SiteId};
use store::download_operations::*;
use store::plugins::*;
use store::{Db, Store, StoreError};

fn version(
    id: &str,
    ver: &str,
    hash: &str,
    key_id: Option<&str>,
    sig_status: &str,
) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: hash.into(),
        version: ver.into(),
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

fn enable_plugin(store: &Store, p: &PluginRecord) -> PluginRecord {
    let op_id = uuid::Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&p.plugin_id, p.revision, op_id)
        .unwrap();
    store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: p.plugin_id.clone(),
                operation_id: op_id,
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

fn corrupt_persisted_grant_kinds(store: &Store, pid: &str, sid1: SiteId, sid2: SiteId) {
    store
        .with_writer(Db::App, |c| {
            c.execute(
                "UPDATE plugin_grants SET object_scope = REPLACE(object_scope, '\"kind\":\"site\"', '\"kind\":\" site \"')
                 WHERE plugin_id = ?1 AND object_scope LIKE ?2",
                rusqlite::params![pid, format!("%{sid1}%")],
            )?;
            c.execute(
                "UPDATE plugin_grants SET object_scope = REPLACE(object_scope, '\"kind\":\"site\"', '\"kind\":\"SITE\"')
                 WHERE plugin_id = ?1 AND object_scope LIKE ?2",
                rusqlite::params![pid, format!("%{sid2}%")],
            )?;
            c.execute(
                "UPDATE plugin_grants SET object_scope = REPLACE(object_scope, '\"kind\":\"downloader\"', '\"kind\":\" downloader \"')
                 WHERE plugin_id = ?1",
                rusqlite::params![pid],
            )?;
            c.execute(
                "UPDATE plugin_grants SET object_scope = REPLACE(object_scope, '\"kind\":\"pool\"', '\"kind\":\" pool \"')
                 WHERE plugin_id = ?1",
                rusqlite::params![pid],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn revoke_and_grant_match_persisted_padded_or_uppercase_object_kind_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    let pid = "org.example.r7a1";
    let p = store
        .register_plugin_version(&version(pid, "1.0.0", "h-r7a1", None, "development"), 0)
        .unwrap();
    let sid1 = SiteId::new();
    let sid2 = SiteId::new();
    let dl_id = DownloaderId::new();

    let p = store
        .grant_plugin_permissions(
            pid,
            &p.package_hash,
            p.revision,
            &[
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Object {
                        kind: "site".into(),
                        id: sid1.to_string(),
                    },
                    sensitive: true,
                },
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Object {
                        kind: "site".into(),
                        id: sid2.to_string(),
                    },
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
                PluginGrant {
                    capability: "kv".into(),
                    scope: PluginGrantScope::Object {
                        kind: "pool".into(),
                        id: "main".into(),
                    },
                    sensitive: false,
                },
            ],
        )
        .unwrap();

    corrupt_persisted_grant_kinds(&store, pid, sid1, sid2);

    let revoked = store
        .revoke_plugin_permissions(
            pid,
            &p.package_hash,
            p.revision,
            &[
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Site(sid1),
                    sensitive: true,
                },
                PluginGrant {
                    capability: "command:site.maintenance".into(),
                    scope: PluginGrantScope::Object {
                        kind: "site".into(),
                        id: sid2.to_string(),
                    },
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
                PluginGrant {
                    capability: "kv".into(),
                    scope: PluginGrantScope::Object {
                        kind: "pool".into(),
                        id: "main".into(),
                    },
                    sensitive: false,
                },
            ],
            None,
        )
        .unwrap();

    assert!(store.get_plugin_grants(pid).unwrap().is_empty());
    let en = enable_plugin(&store, &revoked);
    let auth = store.issue_plugin_state_authority(&en, None).unwrap();
    for sid in [sid1, sid2] {
        assert!(matches!(
            store.authorize_plugin_command(
                &auth,
                "site.maintenance",
                &[PluginCommandTarget::Site(sid)],
            ),
            Err(StoreError::Plugin(PluginError::Denied))
        ));
    }
}

#[test]
fn register_plugin_version_and_staged_register_respect_install_lock_and_active_op() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_plugin_developer_mode(true, 0, "admin").unwrap();

    let pid = "org.example.r7a2";
    let intent = StagedPluginIntent {
        target_plugin_id: pid.into(),
        package_hash: Some("h1".into()),
        version: Some("1.0.0".into()),
        artifact_path: None,
        state_policy: None,
        target_config_digest: None,
        grants: vec![PluginGrant {
            capability: "kv".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        }],
    };
    let begin = store
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(domain::UserId::new()),
            idempotency_key: "r7a2-inst".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/api/v1/plugin-installations".into(),
                body_digest: [9; 32],
            },
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            cause: PluginCommandCause::root("r7a2"),
            intent: intent.clone(),
        })
        .unwrap();
    let _claim = store
        .claim_staged_plugin_operation(
            begin.operation.operation_id,
            begin.operation.revision,
            0,
            10,
        )
        .unwrap();

    // Direct register_plugin_version must not bypass an active staged install lock.
    assert!(matches!(
        store.register_plugin_version(&version(pid, "1.0.0", "h1", None, "development"), 0),
        Err(StoreError::Plugin(PluginError::OperationActive(op))) if op == begin.operation.operation_id
    ));

    // Direct register_plugin_version on an existing plugin with active_operation_id must also fail.
    let pid2 = "org.example.r7a2b";
    let p2 = store
        .register_plugin_version(&version(pid2, "1.0.0", "h2a", None, "development"), 0)
        .unwrap();
    let op2 = uuid::Uuid::new_v4();
    let snap2 = store
        .begin_plugin_enable_checked(pid2, p2.revision, op2)
        .unwrap();
    assert!(matches!(
        store.register_plugin_version(
            &version(pid2, "1.1.0", "h2b", None, "development"),
            snap2.plugin.revision,
        ),
        Err(StoreError::Plugin(PluginError::OperationActive(op))) if op == op2
    ));
}

fn seed_cleaned_boost_fact(store: &Store, dl_id: DownloaderId, site_id: SiteId) {
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-r7a3".into(),
            idempotency_key: "ik-r7a3".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            cause_id: "cause:1".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/r7a3.torrent".into(),
            ownership_tag: "cm-op:op-r7a3".into(),
            info_hash: Some("abcd1234".into()),
            bytes_digest: Some("dig-r7a3".into()),
            verified_bytes: Some(4096),
            request_digest: "req-r7a3".into(),
            now_utc: 100,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "cm-op:op-r7a3".into(),
            info_hash: Some("abcd1234".into()),
            operation_id: "op-r7a3".into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            site_id: Some(site_id),
            enclosure: "https://pt.example/r7a3.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(4096),
            bytes_digest: Some("dig-r7a3".into()),
            now_utc: 100,
        })
        .unwrap();

    let tf = store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-r7a3".into(),
            plugin_id: "builtin.boost".into(),
            quota_key: "pool:main".into(),
            downloader_id: dl_id,
            ownership_tag: "cm-op:op-r7a3".into(),
            info_hash: "abcd1234".into(),
            site_id: Some(site_id),
            state: BoostTaskState::Seeding,
            occupied_bytes: 4096,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: 100,
            expected_revision: None,
            now_utc: 100,
        })
        .unwrap();
    let retained = store
        .mark_boost_task_retained_with_paths(
            "tf-r7a3",
            tf.revision,
            4096,
            Some("{\"paths\":[\"/host/dl/r7a3.mkv\"]}"),
            110,
        )
        .unwrap();
    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-r7a3".into(),
            expected_revision: retained.revision,
            cleanup_plan_handle: "plan-r7a3".into(),
            cleanup_plan_json: "{\"paths\":[\"/host/dl/r7a3.mkv\"]}".into(),
            ttl_secs: 60,
            now_utc: 120,
        })
        .unwrap();
    let cleaned = store
        .complete_boost_cleanup("plan-r7a3", planned.revision, 130)
        .unwrap();
    assert_eq!(cleaned.state, BoostTaskState::Cleaned);
}

#[test]
fn upsert_boost_task_fact_preserves_retained_bytes_when_reactivating_inactive_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    seed_cleaned_boost_fact(&store, dl_id, site_id);

    let reactivated = store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-r7a3-v2".into(),
            plugin_id: "builtin.boost".into(),
            quota_key: "pool:main".into(),
            downloader_id: dl_id,
            ownership_tag: "cm-op:op-r7a3".into(),
            info_hash: "abcd1234".into(),
            site_id: Some(site_id),
            state: BoostTaskState::RetainedOnly,
            occupied_bytes: 0,
            retained_bytes: 4096,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: 140,
            expected_revision: None,
            now_utc: 140,
        })
        .unwrap();
    assert_eq!(reactivated.retained_bytes, 4096);
    let snap = store
        .get_boost_snapshot_facts("pool:main", &[dl_id])
        .unwrap();
    assert_eq!(snap.retained_file_bytes, 4096);

    // Invalid (Downloading with non-zero retained_bytes) over an inactive row must fail CHECK!
    store
        .with_writer(Db::Subscribe, |c| {
            c.execute(
                "UPDATE boost_task_facts SET state='cleaned', occupied_bytes=0, retained_bytes=0 WHERE task_fact_id='tf-r7a3-v2'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
                task_fact_id: "tf-r7a3-v3".into(),
                plugin_id: "builtin.boost".into(),
                quota_key: "pool:main".into(),
                downloader_id: dl_id,
                ownership_tag: "cm-op:op-r7a3".into(),
                info_hash: "abcd1234".into(),
                site_id: Some(site_id),
                state: BoostTaskState::Downloading,
                occupied_bytes: 4096,
                retained_bytes: 999,
                hr_active: Some(false),
                hr_until: None,
                hold_until: None,
                free_until: None,
                protection_state: BoostProtectionState::Eligible,
                protection_reason: None,
                observed_at: 150,
                expected_revision: None,
                now_utc: 150,
            })
            .is_err()
    );
}

#[test]
fn outbox_delivery_component_state_and_lease_renewal_roundtrip() {
    use store::plugin_events::*;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    store
        .emit_outbox_event(
            OutboxDb::App,
            &EmitOutboxEventInput {
                event_id: "ev-r7-1".into(),
                schema_version: 1,
                event_domain: "site".into(),
                event_kind: "site.checkin.after".into(),
                entity_type: "site".into(),
                entity_id: "s1".into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "c1".into(),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: "[]".into(),
                payload_json: "{}".into(),
                occurred_at: 1000,
                created_at: 1000,
            },
        )
        .unwrap();
    store
        .seed_subscriber_deliveries(
            OutboxDb::App,
            &[SeedDeliveryInput {
                event_id: "ev-r7-1".into(),
                plugin_id: "org.example.p".into(),
                next_attempt_at: 1000,
            }],
            1000,
        )
        .unwrap();

    let claimed = store
        .claim_due_deliveries(
            OutboxDb::App,
            &ClaimDueDeliveriesInput {
                plugin_id: None,
                now_ms: 1000,
                lease_duration_ms: 5000,
                limit: Some(10),
            },
        )
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let nonce = claimed[0].claim_nonce.clone();
    assert_eq!(claimed[0].delivery.component_state_json, "{}");

    let renewed = store
        .renew_delivery_lease(
            OutboxDb::App,
            "ev-r7-1",
            "org.example.p",
            &nonce,
            4000,
            6000,
        )
        .unwrap();
    assert_eq!(renewed.lease_until, Some(10000));

    let state_json =
        r#"{"package_hash":"h1","done":{"comp-a":"succeeded","comp-b":"permanent_failed"}}"#;
    let saved = store
        .save_delivery_component_state(
            OutboxDb::App,
            "ev-r7-1",
            "org.example.p",
            &nonce,
            state_json,
            4100,
        )
        .unwrap();
    assert_eq!(saved.component_state_json, state_json);

    store
        .mark_dead_letter(
            OutboxDb::App,
            &MarkDeadLetterInput {
                event_id: "ev-r7-1".into(),
                plugin_id: "org.example.p".into(),
                claim_nonce: nonce,
                error: "comp-b denied".into(),
                now_ms: 4200,
            },
        )
        .unwrap();

    let requeued = store
        .requeue_dead_letter(OutboxDb::App, "ev-r7-1", "org.example.p", 4300, 4300)
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&requeued.component_state_json).unwrap();
    assert_eq!(parsed["done"]["comp-a"], "succeeded");
    assert!(parsed["done"].get("comp-b").is_none());
}
