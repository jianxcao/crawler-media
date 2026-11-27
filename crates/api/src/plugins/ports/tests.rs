use std::sync::Arc;
use std::time::Duration;

use plugin_api::{BrowserPurpose, ErrorCode, GuestRole, HttpRequest, LogLevel};
use plugin_host::{HostPorts, Invocation};
use store::plugins::{PluginCommandCause, PluginConfig, PluginSubject};

use super::{ActiveAuth, ActiveInvocation, HostDeps, StoreHostPorts};
use crate::plugins::PluginInvocationRole;

#[test]
fn store_host_ports_lifecycle_mode_allows_base_and_blocks_state_or_commands() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let state = crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let ports = StoreHostPorts::default();
    let mut inv = Invocation::new("org.example.lc", Duration::from_secs(1));
    inv.role = GuestRole::Lifecycle;
    let _guard = ports.register(
        inv.id().to_string(),
        ActiveInvocation {
            deps: HostDeps::from_state(&state),
            auth: ActiveAuth::Lifecycle {
                plugin_id: "org.example.lc".into(),
                config: Some(PluginConfig {
                    schema_revision: "1".into(),
                    revision: 2,
                    values: serde_json::json!({"mode": "dry"}),
                }),
            },
            subject: PluginSubject::User(domain::UserId::new()),
            cause: PluginCommandCause::root("test-lc"),
            role: PluginInvocationRole::Lifecycle,
            manifest_http: Arc::new(Vec::new()),
            max_response_bytes: None,
        },
    );
    assert!(ports.authorized(&inv, "config"));
    assert!(ports.authorized(&inv, "log"));
    assert!(!ports.authorized(&inv, "kv"));
    assert!(!ports.authorized(&inv, "blob"));
    assert!(!ports.authorized(&inv, "operations"));
    assert!(!ports.authorized(&inv, "command:schedule.set"));
    let cfg = ports.config_get(&inv).unwrap();
    assert_eq!(cfg.revision, 2);
    assert!(ports.log_write(&inv, LogLevel::Info, "ok", &[]).is_ok());
    assert_eq!(
        ports.kv_put(&inv, "k", b"v", None).unwrap_err().code,
        ErrorCode::WrongContext
    );
    assert_eq!(
        ports.blob_delete(&inv, "b", None).unwrap_err().code,
        ErrorCode::WrongContext
    );

    for cap in [
        "http",
        "http:site-auth",
        "read:sites",
        "browser:search",
        "browser:maintenance",
    ] {
        assert!(!ports.authorized(&inv, cap), "expected {cap} unauthorized");
    }
    assert_eq!(
        ports
            .http_fetch(
                &inv,
                HttpRequest {
                    method: "GET".into(),
                    target: plugin_api::HttpTarget::PublicUrl("https://example.com".into()),
                    headers: vec![],
                    body: None,
                    max_bytes: 1024,
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::WrongContext
    );
    assert_eq!(
        ports.site_get(&inv, "s1").unwrap_err().code,
        ErrorCode::WrongContext
    );
    assert_eq!(
        ports
            .browser_open(&inv, BrowserPurpose::Search)
            .err()
            .map(|e| e.code),
        Some(ErrorCode::WrongContext)
    );
}

pub(super) struct NoopFetcher;
impl indexer::Fetcher for NoopFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("noop".into()))
    }
}

#[test]
fn commands_submit_site_maintenance_normalizes_site_id_payload() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let sid = domain::SiteId::new();
    let (_, payload) = super::ops_and_errors::resolve_command_payload(
        &store,
        "org.example.maint",
        None,
        &plugin_api::CommandAction::SiteMaintenance {
            site_handle: format!("site:{sid}"),
            action: plugin_api::MaintenanceAction::CheckIn,
        },
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(json["site_id"], sid.to_string());
    assert_eq!(json["action"], "check-in");
    assert!(json.get("site_handle").is_none());
}

#[test]
fn begin_invocation_propagates_and_enforces_real_plugin_invocation_role() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let state = crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let meta = store::plugins::PluginVersionMetadata {
        plugin_id: "org.example.role".into(),
        package_hash: "role-h1".into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "unused".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    };
    let p = state.store().register_plugin_version(&meta, 0).unwrap();
    let grants: Vec<_> = [
        "http",
        "command:schedule.set",
        "indexer:provide",
        "maintenance:provide",
    ]
    .into_iter()
    .map(|cap| store::plugins::PluginGrant {
        capability: cap.into(),
        scope: store::plugins::PluginGrantScope::Global,
        sensitive: false,
    })
    .collect();
    let p = state
        .store()
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &grants)
        .unwrap();
    let snap = state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    let enabled = state
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
        .unwrap();

    let ports = state.plugins().ports();
    for role in [
        plugin_api::GuestRole::Indexer,
        plugin_api::GuestRole::Maintenance,
        plugin_api::GuestRole::Filter,
        plugin_api::GuestRole::Gate,
        plugin_api::GuestRole::Notify,
    ] {
        let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
        inv.generation = u64::try_from(enabled.generation).unwrap();
        inv.role = role;
        let _g = ports.begin_invocation(&inv).unwrap();
        let ctx = ports.lookup(&inv).unwrap();
        assert_eq!(ctx.role, PluginInvocationRole::from_guest_role(role));
        let cmd_err = ports
            .commands_submit(
                &inv,
                plugin_api::CommandRequest {
                    action: plugin_api::CommandAction::Other {
                        kind: "schedule.set".into(),
                        summary: "x".into(),
                    },
                    idempotency_key: format!("role-{role:?}"),
                    expected_revision: None,
                },
            )
            .unwrap_err();
        assert_eq!(cmd_err.code, ErrorCode::WrongContext);
        if matches!(role, GuestRole::Filter | GuestRole::Gate) {
            let post_err = ports
                .http_fetch(
                    &inv,
                    HttpRequest {
                        method: "POST".into(),
                        target: plugin_api::HttpTarget::PublicUrl("https://example.com".into()),
                        headers: vec![],
                        body: Some(vec![]),
                        max_bytes: 1024,
                    },
                )
                .unwrap_err();
            assert_eq!(post_err.code, ErrorCode::WrongContext);
        }
    }
}

pub(super) fn setup_enabled_plugin(
    state: &crate::ApiState,
    plugin_id: &str,
    grants: Vec<store::plugins::PluginGrant>,
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
        credential_bound: false,
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

fn boost_and_dl_grants() -> Vec<store::plugins::PluginGrant> {
    let mut grants = vec![store::plugins::PluginGrant {
        capability: "read:boost".into(),
        scope: store::plugins::PluginGrantScope::Object {
            kind: "pool".into(),
            id: "default".into(),
        },
        sensitive: false,
    }];
    for cmd in [
        "command:downloader.add",
        "command:downloader.control",
        "command:downloader.delete-task",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
    ] {
        grants.push(store::plugins::PluginGrant {
            capability: cmd.into(),
            scope: store::plugins::PluginGrantScope::Global,
            sensitive: false,
        });
    }
    grants
}

#[test]
fn read_boost_snapshot_and_downloader_commands_validate_live_targets() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let state = crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let enabled = setup_enabled_plugin(&state, "boost.test.plugin", boost_and_dl_grants());
    let ports = state.plugins().ports();

    let mut notify_inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    notify_inv.generation = u64::try_from(enabled.generation).unwrap();
    notify_inv.role = GuestRole::Notify;
    let _ng = ports.begin_invocation(&notify_inv).unwrap();
    assert_eq!(
        ports
            .boost_snapshot(&notify_inv, &[], &[])
            .unwrap_err()
            .code,
        ErrorCode::WrongContext
    );
    drop(_ng);

    for hook_role in [GuestRole::Gate, GuestRole::Filter] {
        let mut hook_inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
        hook_inv.generation = u64::try_from(enabled.generation).unwrap();
        hook_inv.role = hook_role;
        let _hg = ports.begin_invocation(&hook_inv).unwrap();
        assert!(ports.boost_snapshot(&hook_inv, &[], &[]).is_ok());
    }

    let mut task_inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    task_inv.generation = u64::try_from(enabled.generation).unwrap();
    task_inv.role = GuestRole::Task;
    let _tg = ports.begin_invocation(&task_inv).unwrap();
    assert!(ports.authorized(&task_inv, "read:boost"));
    let snap = ports.boost_snapshot(&task_inv, &[], &[]).unwrap();
    assert!(
        matches!(
            snap.disk.status.as_str(),
            "available" | "unavailable" | "unknown"
        ),
        "unexpected disk status: {}",
        snap.disk.status
    );
    assert!(snap.tasks.is_empty());

    let actions = vec![
        (
            plugin_api::CommandAction::DownloaderControl {
                target: plugin_api::OwnedRevision {
                    handle: "t-1".into(),
                    revision: 1,
                },
                control: plugin_api::TaskControl::Pause,
            },
            ErrorCode::NotFound,
        ),
        (
            plugin_api::CommandAction::DownloaderDeleteTask {
                downloader_id: "dl-1".into(),
                revision: 1,
                task_handle: "t-1".into(),
            },
            ErrorCode::InvalidArgument,
        ),
        (
            plugin_api::CommandAction::DownloaderPrepareCleanup {
                targets: vec![plugin_api::OwnedRevision {
                    handle: "t-1".into(),
                    revision: 1,
                }],
                reason: "quota".into(),
            },
            ErrorCode::NotFound,
        ),
        (
            plugin_api::CommandAction::DownloaderDeleteFiles {
                target: plugin_api::OwnedRevision {
                    handle: "t-1".into(),
                    revision: 1,
                },
                plan_handle: "plan-1".into(),
            },
            ErrorCode::NotFound,
        ),
    ];
    for (idx, (action, expected)) in actions.into_iter().enumerate() {
        let err = ports
            .commands_submit(
                &task_inv,
                plugin_api::CommandRequest {
                    action,
                    idempotency_key: format!("dl-live-{idx}"),
                    expected_revision: None,
                },
            )
            .unwrap_err();
        assert_eq!(err.code, expected);
    }
    let subject = ports.lookup(&task_inv).unwrap().subject;
    let ops = state
        .store()
        .list_plugin_operations(&subject, None, Some(20))
        .unwrap();
    assert!(
        ops.is_empty(),
        "Rejected command must not write plugin_operations"
    );
}

#[test]
fn commands_submit_enforces_downloader_add_permission_before_resolution_and_object_scope() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let state = crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();

    // Plugin WITHOUT command:downloader.add grant must receive PermissionDenied (not Unsupported)
    // on direct Rust StoreHostPorts::commands_submit.
    let ungranted = setup_enabled_plugin(&state, "org.example.ungranted", vec![]);
    let ports = state.plugins().ports();
    let mut inv_denied = Invocation::new(ungranted.plugin_id.clone(), Duration::from_secs(5));
    inv_denied.generation = u64::try_from(ungranted.generation).unwrap();
    inv_denied.role = GuestRole::Task;
    let _g1 = ports.begin_invocation(&inv_denied).unwrap();
    let err_denied = ports
        .commands_submit(
            &inv_denied,
            plugin_api::CommandRequest {
                action: plugin_api::CommandAction::DownloaderAdd {
                    downloader_id: "not-even-a-uuid".into(),
                    download_ref: "".into(),
                    options: plugin_api::DownloadOptions::default(),
                },
                idempotency_key: "ungranted-add".into(),
                expected_revision: None,
            },
        )
        .unwrap_err();
    assert_eq!(err_denied.code, ErrorCode::PermissionDenied);
    drop(_g1);

    // Plugin WITH Object { kind: "downloader", id: dl_allowed } passes authorized()
    // and resolves DownloaderAdd for dl_allowed, while rejecting dl_other.
    let dl_allowed = domain::DownloaderId::new();
    let dl_other = domain::DownloaderId::new();
    let scoped = setup_enabled_plugin(
        &state,
        "org.example.scoped",
        vec![store::plugins::PluginGrant {
            capability: "command:downloader.add".into(),
            scope: store::plugins::PluginGrantScope::Object {
                kind: "downloader".into(),
                id: dl_allowed.to_string(),
            },
            sensitive: false,
        }],
    );
    let mut inv_ok = Invocation::new(scoped.plugin_id.clone(), Duration::from_secs(5));
    inv_ok.generation = u64::try_from(scoped.generation).unwrap();
    inv_ok.role = GuestRole::Task;
    let _g2 = ports.begin_invocation(&inv_ok).unwrap();
    assert!(ports.authorized(&inv_ok, "command:downloader.add"));

    let (targets, payload) = super::ops_and_errors::resolve_command_payload(
        state.store().as_ref(),
        &scoped.plugin_id,
        None,
        &plugin_api::CommandAction::DownloaderAdd {
            downloader_id: dl_allowed.to_string(),
            download_ref: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            options: plugin_api::DownloadOptions {
                relative_directory: Some("movies/sub".into()),
                paused: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    assert_eq!(
        targets,
        vec![store::plugins::PluginCommandTarget::Downloader(dl_allowed)]
    );
    let payload_json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(payload_json["downloader_id"], dl_allowed.to_string());
    assert_eq!(payload_json["relative_directory"], "movies/sub");

    let denied_other = ports
        .commands_submit(
            &inv_ok,
            plugin_api::CommandRequest {
                action: plugin_api::CommandAction::DownloaderAdd {
                    downloader_id: dl_other.to_string(),
                    download_ref: "https://pt.example/1.torrent".into(),
                    options: plugin_api::DownloadOptions::default(),
                },
                idempotency_key: "other-dl".into(),
                expected_revision: None,
            },
        )
        .unwrap_err();
    assert_eq!(denied_other.code, ErrorCode::PermissionDenied);

    // Policy roles (Filter | Gate | Notify) forbid KV/Blob writes with WrongContext.
    for role in [GuestRole::Filter, GuestRole::Gate, GuestRole::Notify] {
        let mut policy_inv = Invocation::new(scoped.plugin_id.clone(), Duration::from_secs(5));
        policy_inv.generation = u64::try_from(scoped.generation).unwrap();
        policy_inv.role = role;
        let _pg = ports.begin_invocation(&policy_inv).unwrap();
        for kind in [
            store::plugins::PluginStateKind::Kv,
            store::plugins::PluginStateKind::Blob,
        ] {
            let err_put = ports
                .state_put(&policy_inv, kind, "k", b"v", None)
                .unwrap_err();
            assert_eq!(err_put.code, ErrorCode::WrongContext);
            let err_del = ports
                .state_delete(&policy_inv, kind, "k", None)
                .unwrap_err();
            assert_eq!(err_del.code, ErrorCode::WrongContext);
        }
    }
}

pub(super) fn seed_boost_fact(
    store: &crate::Store,
    plugin_id: &str,
    dl_id: domain::DownloaderId,
    site_id: domain::SiteId,
    hash: &str,
    tag: &str,
    now: i64,
) {
    let op_id = format!("op-{tag}");
    store
        .accept_download_operation(&store::AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{tag}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(plugin_id.into()),
            subject: plugin_id.into(),
            cause_id: "test".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            bytes_digest: Some(format!("dig-{tag}")),
            verified_bytes: Some(1_000),
            request_digest: format!("req-{tag}"),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&store::RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some(plugin_id.into()),
            subject: plugin_id.into(),
            site_id: Some(site_id),
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_state: store::DownloadOwnershipState::Owned,
            provenance_kind: store::DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(1_000),
            bytes_digest: Some(format!("dig-{tag}")),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&store::UpsertBoostTaskFactInput {
            task_fact_id: format!("tf-{tag}"),
            plugin_id: plugin_id.into(),
            downloader_id: dl_id,
            site_id: Some(site_id),
            quota_key: format!("site:{site_id}"),
            ownership_tag: tag.into(),
            info_hash: hash.into(),
            state: store::BoostTaskState::Downloading,
            occupied_bytes: 1_000,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: store::BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap();
}

#[path = "tests_boost.rs"]
mod tests_boost;
