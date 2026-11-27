use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use plugin_api::{
    BrowserPurpose, ErrorCode, GuestRole, HttpRequest, HttpTarget, PluginTrust, SiteTarget,
};
use plugin_host::{HostPorts, Invocation};
use store::plugins::{PluginCommandCause, PluginGrant, PluginGrantScope, PluginSubject};
use uuid::Uuid;

use super::super::super::{ActiveAuth, ActiveInvocation, HostDeps, StoreHostPorts};
use super::super::setup_enabled_plugin;
use super::test_state;
use crate::plugins::PluginInvocationRole;

fn insert_r11_site(store: &crate::Store, enabled: bool) -> domain::SiteId {
    let id = domain::SiteId::new();
    store
        .insert_site(&domain::Site {
            id,
            name: "R11Site".into(),
            url: "https://pt-r11.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=7; pass=sec".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: Some(12),
            cdp_url: None,
            downloader_id: None,
            enabled,
            maintenance_via: domain::MaintenanceVia::Browser,
            browser_search: true,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

#[test]
fn site_relative_http_and_browser_open_enforce_live_plugin_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = test_state(dir.path());
    let pid = "org.example.r11.ports";
    state.admin_trusted_plugins = HashSet::from([pid.to_string()]);
    let site_id = insert_r11_site(state.store().as_ref(), true);
    let obs = state.store().observe_site(site_id).unwrap().unwrap();
    let ep = obs.credential_epoch().storage_key().to_string();

    let grants = vec![
        PluginGrant {
            capability: "http".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "read:sites".into(),
            scope: PluginGrantScope::Site(site_id),
            sensitive: false,
        },
        PluginGrant {
            capability: "browser:search".into(),
            scope: PluginGrantScope::Site(site_id),
            sensitive: true,
        },
    ];
    let enabled = setup_enabled_plugin(&state, pid, grants.clone());
    let auth = state
        .store()
        .issue_plugin_state_authority(&enabled, None)
        .unwrap();

    let ports = StoreHostPorts::default();
    let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = GuestRole::Indexer;
    inv.trust = PluginTrust::AdminTrusted;
    inv.site_id = Some(format!("site:{site_id}"));
    inv.credential_epoch = Some(ep.clone());
    let _g = ports.register(
        inv.id().to_string(),
        ActiveInvocation {
            deps: HostDeps::from_state(&state),
            auth: ActiveAuth::Runtime {
                authority: auth,
                grants,
                invocation_site: Some((site_id, Some(ep))),
            },
            subject: PluginSubject::User(domain::UserId::new()),
            cause: PluginCommandCause::root("r11-ports"),
            role: PluginInvocationRole::Indexer,
            manifest_http: Arc::new(Vec::new()),
            max_response_bytes: None,
        },
    );

    // Transition plugin out of Enabled while site remains enabled
    state
        .store()
        .begin_plugin_lifecycle(&enabled.plugin_id, enabled.revision, Uuid::new_v4(), false)
        .unwrap();

    let http_err = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::SiteRelative(SiteTarget {
                    path: "/torrents.php".into(),
                    query: Vec::new(),
                    auth_slot: None,
                }),
                headers: Vec::new(),
                body: None,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
    assert_eq!(http_err.code, ErrorCode::StaleGeneration);

    let browser_err = match ports.browser_open(&inv, BrowserPurpose::Search) {
        Ok(_) => panic!("expected StaleGeneration when plugin is no longer Enabled"),
        Err(err) => err,
    };
    assert_eq!(browser_err.code, ErrorCode::StaleGeneration);
}

#[test]
fn begin_invocation_rejects_disabled_or_deleted_site_when_credential_epoch_bound() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let disabled_site = insert_r11_site(state.store().as_ref(), false);
    let disabled_ep = state
        .store()
        .observe_site(disabled_site)
        .unwrap()
        .unwrap()
        .credential_epoch()
        .storage_key()
        .to_string();
    let deleted_site = insert_r11_site(state.store().as_ref(), true);
    let deleted_ep = state
        .store()
        .observe_site(deleted_site)
        .unwrap()
        .unwrap()
        .credential_epoch()
        .storage_key()
        .to_string();

    let grants = vec![
        PluginGrant {
            capability: "indexer:provide".into(),
            scope: PluginGrantScope::Site(disabled_site),
            sensitive: false,
        },
        PluginGrant {
            capability: "indexer:provide".into(),
            scope: PluginGrantScope::Site(deleted_site),
            sensitive: false,
        },
    ];
    let enabled = setup_enabled_plugin(&state, "org.example.r11.begin", grants);
    state.store().delete_site(deleted_site).unwrap();

    let ports = state.plugins.ports();
    let mut inv_disabled = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv_disabled.generation = u64::try_from(enabled.generation).unwrap();
    inv_disabled.role = GuestRole::Indexer;
    inv_disabled.site_id = Some(format!("site:{disabled_site}"));
    inv_disabled.credential_epoch = Some(disabled_ep);
    let err_disabled = match ports.begin_invocation(&inv_disabled) {
        Ok(_) => panic!("expected StaleGeneration for disabled site"),
        Err(err) => err,
    };
    assert_eq!(err_disabled.code, ErrorCode::StaleGeneration);

    let mut inv_deleted = Invocation::new(enabled.plugin_id, Duration::from_secs(5));
    inv_deleted.generation = u64::try_from(enabled.generation).unwrap();
    inv_deleted.role = GuestRole::Indexer;
    inv_deleted.site_id = Some(format!("site:{deleted_site}"));
    inv_deleted.credential_epoch = Some(deleted_ep);
    let err_deleted = match ports.begin_invocation(&inv_deleted) {
        Ok(_) => panic!("expected StaleGeneration for deleted site with credential_epoch"),
        Err(err) => err,
    };
    assert_eq!(err_deleted.code, ErrorCode::StaleGeneration);
}

#[test]
fn begin_invocation_without_epoch_rejects_disabled_or_deleted_site_and_ports_enforce_write_gate() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let disabled_site = insert_r11_site(state.store().as_ref(), false);
    let deleted_site = insert_r11_site(state.store().as_ref(), true);
    let grants = vec![
        PluginGrant {
            capability: "indexer:provide".into(),
            scope: PluginGrantScope::Site(disabled_site),
            sensitive: false,
        },
        PluginGrant {
            capability: "indexer:provide".into(),
            scope: PluginGrantScope::Site(deleted_site),
            sensitive: false,
        },
        PluginGrant {
            capability: "kv".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "command:schedule.disable".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
    ];
    let enabled = setup_enabled_plugin(&state, "org.example.r12.ports", grants.clone());
    state.store().delete_site(deleted_site).unwrap();

    let ports = state.plugins.ports();
    for bad_site in [disabled_site, deleted_site] {
        let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
        inv.generation = u64::try_from(enabled.generation).unwrap();
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(format!("site:{bad_site}"));
        inv.credential_epoch = None;
        let err = match ports.begin_invocation(&inv) {
            Ok(_) => panic!("expected StaleGeneration for non-epoch disabled/deleted site"),
            Err(err) => err,
        };
        assert_eq!(err.code, ErrorCode::StaleGeneration);
    }

    let auth = state
        .store()
        .issue_plugin_state_authority(&enabled, None)
        .unwrap();
    let mut task_inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    task_inv.generation = u64::try_from(enabled.generation).unwrap();
    task_inv.role = GuestRole::Task;
    let _g = ports.register(
        task_inv.id().to_string(),
        ActiveInvocation {
            deps: HostDeps::from_state(&state),
            auth: ActiveAuth::Runtime {
                authority: auth,
                grants,
                invocation_site: None,
            },
            subject: PluginSubject::User(domain::UserId::new()),
            cause: PluginCommandCause::root("r12-write-gate"),
            role: PluginInvocationRole::Task,
            manifest_http: Arc::new(Vec::new()),
            max_response_bytes: None,
        },
    );

    let cmd = plugin_api::CommandRequest {
        idempotency_key: "r12-cmd-1".into(),
        expected_revision: None,
        action: plugin_api::CommandAction::ScheduleDisable {
            handle: Uuid::new_v4().to_string(),
            revision: 1,
        },
    };
    {
        let wg = state.write_gate();
        let _closed = wg.close();
        assert_eq!(
            ports.kv_put(&task_inv, "k1", b"v1", None).unwrap_err().code,
            ErrorCode::Busy
        );
        assert_eq!(
            ports
                .commands_submit(&task_inv, cmd.clone())
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
    }

    let ro_state = state.clone().with_instance_writable(false);
    assert!(!ro_state.instance_writable());
    assert_eq!(
        ports.kv_put(&task_inv, "k1", b"v1", None).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        ports.commands_submit(&task_inv, cmd).unwrap_err().code,
        ErrorCode::Conflict
    );
}
