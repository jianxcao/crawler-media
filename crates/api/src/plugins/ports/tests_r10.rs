use std::sync::Arc;
use std::time::Duration;

use plugin_api::{ErrorCode, GuestRole, HttpRequest, HttpTarget, PageRequest};
use plugin_host::{HostPorts, Invocation};
use plugin_package::HttpDestinationManifest;
use store::plugins::{PluginCommandCause, PluginGrant, PluginGrantScope, PluginSubject};
use uuid::Uuid;

use super::super::super::{ActiveAuth, ActiveInvocation, HostDeps, StoreHostPorts};
use super::super::setup_enabled_plugin;
use super::test_state;
use crate::plugins::PluginInvocationRole;

fn insert_test_site(store: &crate::Store, enabled: bool) -> domain::SiteId {
    let id = domain::SiteId::new();
    store
        .insert_site(&domain::Site {
            id,
            name: "R10Site".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=sec".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: Some(12),
            cdp_url: None,
            downloader_id: None,
            enabled,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

#[test]
fn site_and_public_http_ports_and_optional_site_enforce_live_authority_and_site_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path());
    let site_id = insert_test_site(state.store().as_ref(), true);
    let grants = vec![
        PluginGrant {
            capability: "read:sites".into(),
            scope: PluginGrantScope::Site(site_id),
            sensitive: false,
        },
        PluginGrant {
            capability: "http".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
    ];
    let enabled = setup_enabled_plugin(&state, "org.example.r10.ports", grants.clone());
    let auth = state
        .store()
        .issue_plugin_state_authority(&enabled, None)
        .unwrap();

    let ports = StoreHostPorts::default();
    let mut inv = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.register(
        inv.id().to_string(),
        ActiveInvocation {
            deps: HostDeps::from_state(&state),
            auth: ActiveAuth::Runtime {
                authority: auth,
                grants,
                invocation_site: None,
            },
            subject: PluginSubject::User(domain::UserId::new()),
            cause: PluginCommandCause::root("r10-ports"),
            role: PluginInvocationRole::Task,
            manifest_http: Arc::new(vec![HttpDestinationManifest {
                origin: "https://api.example.com".into(),
                path_prefix: "/".into(),
                methods: vec!["GET".into()],
            }]),
            max_response_bytes: None,
        },
    );

    // Transition plugin out of Enabled in SQLite directly
    state
        .store()
        .begin_plugin_lifecycle(&enabled.plugin_id, enabled.revision, Uuid::new_v4(), false)
        .unwrap();

    let handle = format!("site:{site_id}");
    assert_eq!(
        ports
            .site_list(
                &inv,
                PageRequest {
                    cursor: None,
                    limit: 10
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::StaleGeneration
    );
    assert_eq!(
        ports.site_get(&inv, &handle).unwrap_err().code,
        ErrorCode::StaleGeneration
    );
    assert_eq!(
        ports.site_stats(&inv, &handle).unwrap_err().code,
        ErrorCode::StaleGeneration
    );
    let http_err = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::PublicUrl("https://api.example.com/ping".into()),
                headers: vec![],
                body: None,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
    assert_eq!(http_err.code, ErrorCode::StaleGeneration);

    // Also verify try_resolve_optional_site rejects disabled/deleted sites when credential_epoch is present
    let disabled_site = insert_test_site(state.store().as_ref(), false);
    let obs = state.store().observe_site(disabled_site).unwrap().unwrap();
    let mut inv_opt = Invocation::new("org.example.r10.opt", Duration::from_secs(5));
    inv_opt.site_id = Some(disabled_site.to_string());
    inv_opt.credential_epoch = Some(obs.credential_epoch().storage_key().to_string());
    let opt_err =
        StoreHostPorts::try_resolve_optional_site(&HostDeps::from_state(&state), &inv_opt)
            .unwrap_err();
    assert_eq!(opt_err.code, ErrorCode::StaleGeneration);
}
