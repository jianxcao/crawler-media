use std::sync::Arc;
use std::time::Duration;

use plugin_api::{
    CommandAction, CommandRequest, DownloadOptions, ErrorCode, GuestRole, OwnedRevision,
};
use plugin_host::{HostPorts, Invocation};
use store::plugins::{PluginGrant, PluginGrantScope};
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, PrepareBoostCleanupPlanInput, RecordDownloadOwnershipInput,
    UpsertBoostTaskFactInput,
};

use super::{NoopFetcher, boost_and_dl_grants, seed_boost_fact, setup_enabled_plugin};

fn test_state(dir: &std::path::Path) -> crate::ApiState {
    let store = Arc::new(crate::Store::open(dir.join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.join("dl")));
    crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap()
}

#[test]
fn global_grant_authorizes_downloader_add_via_host_ports() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let enabled = setup_enabled_plugin(&state, "boost.global.dl", boost_and_dl_grants());
    let ports = state.plugins().ports();
    let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();

    let dl_id = domain::DownloaderId::new();
    let op_id = ports
        .commands_submit(
            &inv,
            CommandRequest {
                action: CommandAction::DownloaderAdd {
                    downloader_id: dl_id.to_string(),
                    download_ref: "https://pt.example/global.torrent".into(),
                    options: DownloadOptions::default(),
                },
                idempotency_key: "global-add-1".into(),
                expected_revision: None,
            },
        )
        .unwrap();
    assert!(!op_id.is_empty());
}

#[test]
fn read_boost_malformed_or_unknown_object_scope_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let dl_id = domain::DownloaderId::new();
    let site_id = domain::SiteId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "dl".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();

    for (pid, kind, id) in [
        ("bad.site.scope", "site", "not-a-uuid".to_string()),
        ("bad.dl.scope", "downloader", "not-a-uuid".to_string()),
        (
            "bad.kind.scope",
            "library",
            uuid::Uuid::new_v4().to_string(),
        ),
    ] {
        let p = setup_enabled_plugin(
            &state,
            pid,
            vec![PluginGrant {
                capability: "read:boost".into(),
                scope: PluginGrantScope::Object {
                    kind: kind.into(),
                    id,
                },
                sensitive: false,
            }],
        );
        seed_boost_fact(
            state.store().as_ref(),
            &p.plugin_id,
            dl_id,
            site_id,
            "1111111111111111111111111111111111111111",
            &format!("tag-{pid}"),
            1_700_000_000,
        );
        let ports = state.plugins().ports();
        let mut inv = Invocation::new(p.plugin_id.clone(), Duration::from_secs(5));
        inv.generation = u64::try_from(p.generation).unwrap();
        inv.role = GuestRole::Task;
        let _g = ports.begin_invocation(&inv).unwrap();
        assert!(
            !ports.authorized(&inv, "read:boost"),
            "{pid} must fail authorized"
        );
        let err = ports.boost_snapshot(&inv, &[], &[]).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::PermissionDenied,
            "{pid} must fail closed"
        );
    }
}

#[test]
fn prepare_cleanup_rejects_multi_target_via_host_ports() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let enabled = setup_enabled_plugin(&state, "boost.multi.prep", boost_and_dl_grants());
    let dl_id = domain::DownloaderId::new();
    let site_id = domain::SiteId::new();
    seed_boost_fact(
        state.store().as_ref(),
        &enabled.plugin_id,
        dl_id,
        site_id,
        "aaaa111111111111111111111111111111111111",
        "m1",
        1_700_000_000,
    );
    seed_boost_fact(
        state.store().as_ref(),
        &enabled.plugin_id,
        dl_id,
        site_id,
        "bbbb111111111111111111111111111111111111",
        "m2",
        1_700_000_000,
    );
    let ports = state.plugins().ports();
    let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();

    let err = ports
        .commands_submit(
            &inv,
            CommandRequest {
                action: CommandAction::DownloaderPrepareCleanup {
                    targets: vec![
                        OwnedRevision {
                            handle: "tf-m1".into(),
                            revision: 1,
                        },
                        OwnedRevision {
                            handle: "tf-m2".into(),
                            revision: 1,
                        },
                    ],
                    reason: "quota".into(),
                },
                idempotency_key: "multi-prep-1".into(),
                expected_revision: None,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    let subject = ports.lookup(&inv).unwrap().subject;
    assert!(
        state
            .store()
            .list_plugin_operations(&subject, None, Some(10))
            .unwrap()
            .is_empty()
    );
}

fn seed_cleanup_planned_task(
    store: &crate::Store,
    plugin_id: &str,
    dl_id: domain::DownloaderId,
    site_id: domain::SiteId,
    stage: &std::path::Path,
    victim: &std::path::Path,
    now: i64,
) -> store::BoostTaskFactRecord {
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-gc".into(),
            idempotency_key: "ik-gc".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(plugin_id.into()),
            subject: plugin_id.into(),
            cause_id: "c".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/gc.torrent".into(),
            ownership_tag: "tag-gc".into(),
            info_hash: Some("cccc111111111111111111111111111111111111".into()),
            bytes_digest: Some("dig-gc".into()),
            verified_bytes: Some(7),
            request_digest: "req-gc".into(),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "tag-gc".into(),
            info_hash: Some("cccc111111111111111111111111111111111111".into()),
            operation_id: "op-gc".into(),
            subscribe_id: None,
            plugin_id: Some(plugin_id.into()),
            subject: plugin_id.into(),
            site_id: Some(site_id),
            enclosure: "https://pt.example/gc.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(7),
            bytes_digest: Some("dig-gc".into()),
            now_utc: now,
        })
        .unwrap();
    let tf = store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-gc".into(),
            plugin_id: plugin_id.into(),
            quota_key: format!("{plugin_id}:{dl_id}"),
            downloader_id: dl_id,
            ownership_tag: "tag-gc".into(),
            info_hash: "cccc111111111111111111111111111111111111".into(),
            site_id: Some(site_id),
            state: BoostTaskState::Seeding,
            occupied_bytes: 7,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap();
    let plan_json = serde_json::json!({
        "save_path": stage.to_string_lossy(),
        "allowed_roots": [stage.to_string_lossy()],
        "paths": [victim.to_string_lossy()],
    })
    .to_string();
    let ret = store
        .mark_boost_task_retained_with_paths(
            &tf.task_fact_id,
            tf.revision,
            7,
            Some(&plan_json),
            now,
        )
        .unwrap();
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf.task_fact_id,
            expected_revision: ret.revision,
            cleanup_plan_handle: "plan-gc".into(),
            cleanup_plan_json: plan_json,
            ttl_secs: 600,
            now_utc: now,
        })
        .unwrap()
}

#[test]
fn guest_task_command_delete_files_succeeds_without_confirmation_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let stage = dir.path().join("dl");
    std::fs::create_dir_all(&stage).unwrap();
    let victim = stage.join("guest-clean.mkv");
    std::fs::write(&victim, b"payload").unwrap();

    let enabled = setup_enabled_plugin(&state, "boost.guest.clean", boost_and_dl_grants());
    let dl_id = domain::DownloaderId::new();
    let site_id = domain::SiteId::new();
    let now = crate::job_loop::unix_now();
    let planned = seed_cleanup_planned_task(
        state.store().as_ref(),
        &enabled.plugin_id,
        dl_id,
        site_id,
        &stage,
        &victim,
        now,
    );

    let ports = state.plugins().ports();
    let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();
    let op_id = ports
        .commands_submit(
            &inv,
            CommandRequest {
                action: CommandAction::DownloaderDeleteFiles {
                    target: OwnedRevision {
                        handle: planned.task_fact_id,
                        revision: u64::try_from(planned.revision).unwrap(),
                    },
                    plan_handle: "plan-gc".into(),
                },
                idempotency_key: "guest-df-1".into(),
                expected_revision: None,
            },
        )
        .unwrap();

    crate::run_plugin_command_for_test(&state, op_id.parse().unwrap()).unwrap();
    let op = state
        .store()
        .get_plugin_operation(&ports.lookup(&inv).unwrap().subject, op_id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(op.state, store::plugins::PluginOperationState::Succeeded);
    assert!(!victim.exists());
}

fn setup_bound_plugin(
    state: &crate::ApiState,
    plugin_id: &str,
    grants: Vec<PluginGrant>,
) -> store::plugins::PluginRecord {
    let meta = store::plugins::PluginVersionMetadata {
        plugin_id: plugin_id.into(),
        package_hash: format!("{plugin_id}-h1"),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "unused".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: true,
    };
    let p = state.store().register_plugin_version(&meta, 0).unwrap();
    let p = state
        .store()
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &grants)
        .unwrap();
    let snap = state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    state
        .store()
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: snap.plugin_id,
            revision: snap.revision,
            generation: snap.generation,
            operation_id: snap.active_operation_id.unwrap(),
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

fn seed_dl_and_two_sites(
    store: &crate::Store,
) -> (domain::DownloaderId, domain::SiteId, domain::SiteId) {
    let dl_1 = domain::DownloaderId::new();
    let site_1 = domain::SiteId::new();
    let site_2 = domain::SiteId::new();
    store
        .insert_downloader(&store::DownloaderRow {
            id: dl_1,
            name: "dl1".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    for (sid, name) in [(site_1, "s1"), (site_2, "s2")] {
        store
            .insert_site(&domain::Site {
                id: sid,
                name: name.into(),
                url: format!("https://{name}.example/"),
                profile_id: "mteam".into(),
                cookie: Some("uid=1".into()),
                api_key: None,
                rss_url: None,
                proxy: None,
                rate_limit_per_minute: Some(60),
                cdp_url: None,
                downloader_id: None,
                enabled: true,
                maintenance_via: domain::MaintenanceVia::Http,
                browser_search: false,
                timezone: "UTC".into(),
            })
            .unwrap();
    }
    (dl_1, site_1, site_2)
}

#[test]
fn credential_bound_read_boost_downloader_only_and_mismatched_site_isolate_other_sites() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let (dl_1, site_1, site_2) = seed_dl_and_two_sites(state.store().as_ref());

    let bound_dl = setup_bound_plugin(
        &state,
        "bound.dl.only",
        vec![PluginGrant {
            capability: "read:boost".into(),
            scope: PluginGrantScope::Object {
                kind: "downloader".into(),
                id: dl_1.to_string(),
            },
            sensitive: false,
        }],
    );
    let now = 1_700_000_000i64;
    seed_boost_fact(
        state.store().as_ref(),
        &bound_dl.plugin_id,
        dl_1,
        site_1,
        "1111111111111111111111111111111111111111",
        "s1",
        now,
    );
    seed_boost_fact(
        state.store().as_ref(),
        &bound_dl.plugin_id,
        dl_1,
        site_2,
        "2222222222222222222222222222222222222222",
        "s2",
        now,
    );

    let ports = state.plugins().ports();
    let mut inv = Invocation::new(bound_dl.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(bound_dl.generation).unwrap();
    inv.role = GuestRole::Task;
    inv.site_id = Some(format!("site:{site_1}"));
    let _g1 = ports.begin_invocation(&inv).unwrap();
    let snap = ports.boost_snapshot(&inv, &[], &[]).unwrap();
    assert_eq!(snap.tasks.len(), 1, "tasks={:?}", snap.tasks);
    assert_eq!(snap.tasks[0].handle, "tf-s1");
    assert_eq!(snap.used_bytes, 1_000);
    assert_eq!(
        ports
            .boost_snapshot(&inv, &[site_2.to_string()], &[])
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    drop(_g1);

    let wrong_site = setup_bound_plugin(
        &state,
        "bound.dl.wrong_site",
        vec![
            PluginGrant {
                capability: "read:boost".into(),
                scope: PluginGrantScope::Site(site_2),
                sensitive: false,
            },
            PluginGrant {
                capability: "read:boost".into(),
                scope: PluginGrantScope::Object {
                    kind: "downloader".into(),
                    id: dl_1.to_string(),
                },
                sensitive: false,
            },
        ],
    );
    let mut inv2 = Invocation::new(wrong_site.plugin_id.clone(), Duration::from_secs(5));
    inv2.generation = u64::try_from(wrong_site.generation).unwrap();
    inv2.role = GuestRole::Task;
    inv2.site_id = Some(format!("site:{site_1}"));
    let _g2 = ports.begin_invocation(&inv2).unwrap();
    assert_eq!(
        ports.boost_snapshot(&inv2, &[], &[]).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
}

#[test]
fn captured_cookies_evicts_oldest_entry_without_wiping_book_at_capacity() {
    let ports = super::StoreHostPorts::default();
    let site_id = domain::SiteId::new();
    let origin = net_guard::normalize_url("https://pt.example", false)
        .unwrap()
        .origin;
    let mut tokens = Vec::with_capacity(1025);
    for idx in 0..1025usize {
        let tok = ports.record_captured_cookies(crate::plugins::ports::CapturedCookieRecord {
            site_id,
            credential_epoch: format!("ep-{idx}"),
            origin: origin.clone(),
            set_cookies: vec![format!("c={idx}")],
            merged_cookie: Some(format!("c={idx}")),
        });
        tokens.push(tok);
    }
    assert!(
        ports.take_captured_cookies(&tokens[0]).is_none(),
        "oldest entry must be evicted at 1024 capacity"
    );
    assert_eq!(
        ports
            .take_captured_cookies(&tokens[1])
            .unwrap()
            .credential_epoch,
        "ep-1"
    );
    assert_eq!(
        ports
            .take_captured_cookies(&tokens[1024])
            .unwrap()
            .credential_epoch,
        "ep-1024"
    );
}

#[path = "tests_r10.rs"]
mod tests_r10;
mod tests_r11;
