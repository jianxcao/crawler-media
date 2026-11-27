#![allow(dead_code)]
use domain::{MaintenanceVia, Site, SiteId};
use store::{Store, plugins::*};

pub fn site() -> Site {
    Site {
        id: SiteId::new(),
        name: "Fixture".into(),
        url: "https://fixture.invalid/".into(),
        profile_id: "fixture".into(),
        cookie: Some("fixture-cookie".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}
pub fn metadata(id: &str, bound: bool) -> PluginVersionMetadata {
    PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: "fixture-digest".into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "/never-read/package".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: bound,
    }
}
pub fn install(store: &Store, id: &str, sites: &[Site]) -> PluginRecord {
    let p = store
        .register_plugin_version(&metadata(id, !sites.is_empty()), 0)
        .unwrap();
    let scopes = if sites.is_empty() {
        vec![PluginGrantScope::Global]
    } else {
        sites.iter().map(|s| PluginGrantScope::Site(s.id)).collect()
    };
    let grants: Vec<_> = scopes
        .into_iter()
        .flat_map(|scope| {
            ["kv", "blob", "command:fixture"].map(|capability| PluginGrant {
                capability: capability.into(),
                scope: scope.clone(),
                sensitive: false,
            })
        })
        .collect();
    store
        .replace_plugin_grants(id, &p.package_hash, p.revision, &grants)
        .unwrap()
}
pub fn evidence(p: &PluginRecord) -> PluginLifecycleObservation {
    PluginLifecycleObservation {
        plugin_id: p.plugin_id.clone(),
        operation_id: p.active_operation_id.unwrap(),
        revision: p.revision,
        generation: p.generation,
        workers_exited: true,
        unknown_effects_reconciled: true,
        validation_succeeded: true,
    }
}
pub fn enable(store: &Store, p: &PluginRecord) -> PluginRecord {
    let p = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    store.finalize_plugin_lifecycle(&evidence(&p)).unwrap()
}
pub fn disable(store: &Store, p: &PluginRecord) -> PluginRecord {
    let p = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    store.finalize_plugin_lifecycle(&evidence(&p)).unwrap()
}
pub fn request(p: &PluginRecord, subject: PluginSubject, key: &str) -> AcceptPluginOperation {
    AcceptPluginOperation {
        plugin_id: Some(p.plugin_id.clone()),
        subject,
        idempotency_key: key.into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/fixture/command".into(),
            body_digest: [4; 32],
        },
        kind: "fixture".into(),
        expected_revision: Some(p.revision),
        expected_generation: Some(p.generation),
        cause: PluginCommandCause::root(format!("cause-{key}")),
    }
}
pub fn outcome(state: PluginOperationState) -> PluginCommandOutcome {
    PluginCommandOutcome {
        state,
        phase: "fixture-complete".into(),
        next_run_at: None,
        result: None,
        effect: if state == PluginOperationState::Unknown {
            PluginEffectObservation::Unknown
        } else {
            PluginEffectObservation::NoEffect
        },
    }
}

pub fn fixture_command(
    store: &Store,
    state: &PluginStateAuthority,
    site: Option<SiteId>,
) -> PluginCommandAuthority {
    let target = site.map_or(PluginCommandTarget::Global, PluginCommandTarget::Site);
    store
        .authorize_plugin_command(state, "fixture", &[target])
        .unwrap()
}
