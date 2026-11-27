use std::sync::Arc;
use std::time::Duration;

use plugin_api::{ErrorCode, GuestRole};
use plugin_host::{HostPorts, Invocation};
use store::plugins::{CanonicalPluginRequest, PluginGrant, PluginGrantScope};

use super::tests::{NoopFetcher, seed_boost_fact, setup_enabled_plugin};

#[test]
fn read_boost_grant_enforces_object_and_site_scope_filtering() {
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

    let s_allowed = domain::SiteId::new();
    let s_denied = domain::SiteId::new();
    let dl_allowed = domain::DownloaderId::new();

    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_allowed,
            name: "dl-allowed".into(),
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

    let dl_scoped = setup_enabled_plugin(
        &state,
        "boost.dl.scoped",
        vec![PluginGrant {
            capability: "read:boost".into(),
            scope: PluginGrantScope::Object {
                kind: "site".into(),
                id: s_allowed.to_string(),
            },
            sensitive: false,
        }],
    );

    let now = 1_700_000_000i64;
    seed_boost_fact(
        state.store().as_ref(),
        &dl_scoped.plugin_id,
        dl_allowed,
        s_allowed,
        "1111111111111111111111111111111111111111",
        "tag-allow",
        now,
    );
    seed_boost_fact(
        state.store().as_ref(),
        &dl_scoped.plugin_id,
        dl_allowed,
        s_denied,
        "2222222222222222222222222222222222222222",
        "tag-deny",
        now,
    );

    let ports = state.plugins().ports();
    let mut inv = Invocation::new(dl_scoped.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(dl_scoped.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();
    assert!(ports.authorized(&inv, "read:boost"));
    let snap = ports.boost_snapshot(&inv, &[], &[]).unwrap();
    assert_eq!(snap.tasks.len(), 1);
    assert_eq!(snap.tasks[0].handle, "tf-tag-allow");

    // Passing site_denied explicitly returns PermissionDenied on read:boost Object{kind:site}.
    let err = ports
        .boost_snapshot(&inv, &[s_denied.to_string()], &[])
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
}

#[test]
fn downloader_commands_enforce_permission_before_supports_kind_on_rust_and_service_paths() {
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
    let no_dl_perm = setup_enabled_plugin(
        &state,
        "no.dl.perms",
        vec![PluginGrant {
            capability: "read:boost".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        }],
    );
    let ports = state.plugins().ports();
    let mut inv = Invocation::new(no_dl_perm.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(no_dl_perm.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();

    for (idx, action) in [
        plugin_api::CommandAction::DownloaderControl {
            target: plugin_api::OwnedRevision {
                handle: "not-even-resolved".into(),
                revision: 1,
            },
            control: plugin_api::TaskControl::Pause,
        },
        plugin_api::CommandAction::DownloaderDeleteTask {
            downloader_id: "invalid-uuid".into(),
            revision: 1,
            task_handle: "t-1".into(),
        },
        plugin_api::CommandAction::DownloaderPrepareCleanup {
            targets: vec![],
            reason: "quota".into(),
        },
        plugin_api::CommandAction::DownloaderDeleteFiles {
            target: plugin_api::OwnedRevision {
                handle: "t-1".into(),
                revision: 1,
            },
            plan_handle: "missing".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let err = ports
            .commands_submit(
                &inv,
                plugin_api::CommandRequest {
                    action,
                    idempotency_key: format!("deny-first-{idx}"),
                    expected_revision: None,
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    let err_admin = rt
        .block_on(state.plugins().submit_admin_command(
            &state,
            &no_dl_perm.plugin_id,
            crate::plugins::SubmitAdminCommandInput {
                actor_user_id: domain::UserId::new(),
                kind: "downloader.delete-files".into(),
                targets: vec![store::plugins::PluginCommandTarget::Global],
                payload: serde_json::json!({ "task_id": "t-1", "plan_handle": "p-1" }),
                evidence_ref: None,
                idempotency_key: "admin-deny-first".into(),
                expected_revision: no_dl_perm.revision,
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/api/v1/plugins/{}/commands", no_dl_perm.plugin_id),
                    body_digest: [1u8; 32],
                },
            },
        ))
        .unwrap_err();
    assert!(matches!(
        err_admin,
        crate::plugins::PluginServiceError::Store(store::StoreError::Plugin(
            store::plugins::PluginError::Denied
        ))
    ));
}

struct CountingStatsFetcher {
    calls: std::sync::atomic::AtomicUsize,
    body: std::sync::Mutex<String>,
}

impl indexer::Fetcher for CountingStatsFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.body.lock().unwrap().clone())
    }
}

#[test]
fn site_stats_fetches_live_stats_caches_for_ttl_and_invalidates_on_epoch_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let fetcher = Arc::new(CountingStatsFetcher {
        calls: std::sync::atomic::AtomicUsize::new(0),
        body: std::sync::Mutex::new(
            "<html><body>上传量：2.00 TiB 下载量：1.00 TiB</body></html>".into(),
        ),
    });
    let state = crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        fetcher.clone(),
        dl,
    )
    .unwrap();

    let site_id = domain::SiteId::new();
    let site = domain::Site {
        id: site_id,
        name: "Audiences".into(),
        url: "https://pt.example/".into(),
        profile_id: "audiences".into(),
        cookie: Some("c_secure_uid=1; c_secure_pass=v1".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: Some(600),
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
        browser_search: false,
        timezone: "Asia/Shanghai".into(),
    };
    state.store().insert_site(&site).unwrap();

    let plugin = setup_enabled_plugin(
        &state,
        "org.example.stats",
        vec![PluginGrant {
            capability: "read:sites".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        }],
    );
    let ports = state.plugins().ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();

    let first = ports.site_stats(&inv, &site_id.to_string()).unwrap();
    assert_eq!(first.uploaded, Some(2 * 1024_u64.pow(4)));
    assert_eq!(first.downloaded, Some(1024_u64.pow(4)));
    assert_eq!(fetcher.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    let second = ports.site_stats(&inv, &site_id.to_string()).unwrap();
    assert_eq!(second, first);
    assert_eq!(
        fetcher.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "second call within TTL should hit cache"
    );

    *fetcher.body.lock().unwrap() =
        "<html><body>Uploaded: 3.00 TiB Downloaded: 1.00 TiB</body></html>".into();
    let mut updated_site = site.clone();
    updated_site.cookie = Some("c_secure_uid=1; c_secure_pass=v2".into());
    state
        .store()
        .save_site(&updated_site, store::ManualCredentialWrite::Replace)
        .unwrap();

    let refreshed = ports.site_stats(&inv, &site_id.to_string()).unwrap();
    assert_eq!(refreshed.uploaded, Some(3 * 1024_u64.pow(4)));
    assert_eq!(fetcher.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn boost_run_cycle_port_enforces_task_role_and_permission_grant() {
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
    let plugin = setup_enabled_plugin(
        &state,
        "org.example.boostcycle",
        vec![
            PluginGrant {
                capability: "read:boost".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            },
            PluginGrant {
                capability: "read:sites".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            },
            PluginGrant {
                capability: "command:downloader.add".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            },
        ],
    );
    let ports = state.plugins().ports();
    let mut gate_inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    gate_inv.generation = u64::try_from(plugin.generation).unwrap();
    gate_inv.role = GuestRole::Gate;
    let _gg = ports.begin_invocation(&gate_inv).unwrap();
    assert_eq!(
        ports.boost_run_cycle(&gate_inv, "{}").unwrap_err().code,
        ErrorCode::WrongContext
    );
    drop(_gg);

    let mut task_inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    task_inv.generation = u64::try_from(plugin.generation).unwrap();
    task_inv.role = GuestRole::Task;
    let _tg = ports.begin_invocation(&task_inv).unwrap();
    assert_eq!(
        ports.boost_run_cycle(&task_inv, "{}").unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    drop(_tg);

    let site_id = domain::SiteId::from_uuid(uuid::Uuid::new_v4());
    let other_site_id = domain::SiteId::from_uuid(uuid::Uuid::new_v4());
    let scoped_plugin = setup_enabled_plugin(
        &state,
        "org.example.scopedboost",
        vec![
            PluginGrant {
                capability: "read:boost".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            },
            PluginGrant {
                capability: "read:sites".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            },
            PluginGrant {
                capability: "command:downloader.add".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            },
            PluginGrant {
                capability: "command:boost.run".into(),
                scope: PluginGrantScope::Site(site_id),
                sensitive: false,
            },
        ],
    );
    let mut scoped_inv = Invocation::new(scoped_plugin.plugin_id.clone(), Duration::from_secs(5));
    scoped_inv.generation = u64::try_from(scoped_plugin.generation).unwrap();
    scoped_inv.role = GuestRole::Task;
    let _sg = ports.begin_invocation(&scoped_inv).unwrap();
    let _tsg = ports.register_task_state(scoped_inv.id().to_string(), state.clone());
    assert_eq!(
        ports
            .boost_run_cycle(
                &scoped_inv,
                &format!(r#"{{"dry_run":true,"sites":["{other_site_id}"]}}"#)
            )
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
}
