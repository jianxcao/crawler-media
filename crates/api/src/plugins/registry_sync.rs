use std::sync::Arc;

use indexer::{
    AuthType, MaintenanceAction, MaintenanceDescriptor, ProviderAvailability, ProviderCapability,
    ProviderDescriptor, ProviderSource, RegistryCandidate,
};
use plugin_host::{
    INDEXER_WORLD, MAINTENANCE_WORLD, PluginHost, WasmIndexerConfig, WasmIndexerProvider,
    WasmMaintenanceConfig, WasmMaintenanceProvider,
};
use plugin_package::PluginManifest;
use store::Store;
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycle, PluginRecord, PluginVersionMetadata,
};

use super::errors::PluginServiceError;
use super::staging::safe_rel_path;
use super::wasm_runner::{resolve_artifact_path, resolve_plugin_trust};
use crate::management::ApiState;

fn parse_provider_capabilities(caps: &[String]) -> Result<Vec<ProviderCapability>, String> {
    caps.iter()
        .map(|raw| match raw.as_str() {
            "search" => Ok(ProviderCapability::Search),
            "rss" => Ok(ProviderCapability::Rss),
            "detail" => Ok(ProviderCapability::Detail),
            "resolve-download" => Ok(ProviderCapability::ResolveDownload),
            "user-stats" => Ok(ProviderCapability::UserStats),
            "categories" => Ok(ProviderCapability::Categories),
            other => Err(format!("unsupported indexer capability `{other}`")),
        })
        .collect()
}

fn parse_maintenance_actions(actions: &[String]) -> Result<Vec<MaintenanceAction>, String> {
    actions
        .iter()
        .map(|raw| match raw.as_str() {
            "verify-session" => Ok(MaintenanceAction::VerifySession),
            "check-in" => Ok(MaintenanceAction::CheckIn),
            other => Err(format!("unsupported maintenance action `{other}`")),
        })
        .collect()
}

fn infer_auth_types(permissions: &[String], credential_bound: bool) -> Vec<AuthType> {
    if credential_bound || permissions.iter().any(|p| p == "http:site-auth") {
        vec![AuthType::Cookie, AuthType::ApiKey]
    } else {
        vec![AuthType::None]
    }
}

fn manifest_contributes_providers(manifest: &PluginManifest) -> bool {
    !manifest.sites.is_empty()
        || manifest
            .components
            .iter()
            .any(|c| c.world == INDEXER_WORLD || c.world == MAINTENANCE_WORLD)
}

fn load_enabled_plugins(
    store: &Store,
) -> Result<Vec<(PluginRecord, PluginVersionMetadata)>, PluginServiceError> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page =
            store.list_plugins_page(Some(PluginLifecycle::Enabled), after.as_deref(), Some(200))?;
        let next = page.next_cursor.clone();
        for plugin in page.items {
            if let Some(version) =
                store.get_plugin_version(&plugin.plugin_id, &plugin.package_hash)?
            {
                out.push((plugin, version));
            }
        }
        match next {
            Some(cursor) if Some(&cursor) != after.as_ref() => after = Some(cursor),
            _ => break,
        }
    }
    Ok(out)
}

fn add_plugin_providers(
    state: &ApiState,
    host: &PluginHost,
    mut candidate: RegistryCandidate,
    plugin: &PluginRecord,
    version: &PluginVersionMetadata,
) -> Result<RegistryCandidate, String> {
    if version.manifest_json == "{}" {
        return Ok(candidate);
    }
    let manifest: PluginManifest = serde_json::from_str(&version.manifest_json)
        .map_err(|e| format!("corrupt manifest_json for `{}`: {e}", plugin.plugin_id))?;
    if !manifest_contributes_providers(&manifest) {
        return Ok(candidate);
    }

    let plugin_gen = u64::try_from(plugin.generation).unwrap_or(0);
    let grants = state
        .store
        .get_plugin_grants(&plugin.plugin_id)
        .map_err(|e| e.to_string())?;
    if version.signature_status == "builtin" {
        if plugin.plugin_id == super::builtins::BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID
            && has_provider_grant(&grants, "maintenance:provide")
        {
            let provider = super::builtins::BuiltinNexusPhpMaintenanceProvider::new(
                plugin_gen,
                Arc::downgrade(&state.indexer),
                state.browser.clone(),
                state.store.clone(),
                state.plugins.ports().clone(),
            );
            candidate = candidate
                .add_maintenance_provider(Arc::new(provider))
                .map_err(|e| e.to_string())?;
        }
        return Ok(candidate);
    }

    let base_dir = resolve_artifact_path(state.store.data_dir(), &version.artifact_path);
    let trust = resolve_plugin_trust(
        &plugin.plugin_id,
        &version.signature_status,
        &state.admin_trusted_plugins,
    );

    for rel in &manifest.sites {
        let full = safe_rel_path(&base_dir, rel).map_err(|e| e.to_string())?;
        let yaml = std::fs::read_to_string(&full)
            .map_err(|e| format!("failed to read plugin site yaml `{rel}`: {e}"))?;
        candidate = candidate
            .add_plugin_yaml(&plugin.plugin_id, &version.version, plugin_gen, &yaml)
            .map_err(|e| e.to_string())?;
    }

    for comp in &manifest.components {
        if comp.world == INDEXER_WORLD {
            if !comp.permissions.iter().any(|p| p == "indexer:provide")
                || !has_provider_grant(&grants, "indexer:provide")
            {
                continue;
            }
            candidate = add_wasm_indexer_component(
                state, host, candidate, plugin, version, comp, plugin_gen, trust,
            )?;
        } else if comp.world == MAINTENANCE_WORLD {
            if !comp.permissions.iter().any(|p| p == "maintenance:provide")
                || !has_provider_grant(&grants, "maintenance:provide")
            {
                continue;
            }
            candidate = add_wasm_maintenance_component(
                state, host, candidate, plugin, version, comp, plugin_gen, trust,
            )?;
        }
    }
    Ok(candidate)
}

fn has_provider_grant(grants: &[PluginGrant], capability: &str) -> bool {
    grants.iter().any(|g| {
        g.capability == capability
            && match &g.scope {
                PluginGrantScope::Global | PluginGrantScope::Site(_) => true,
                PluginGrantScope::Object { kind, id } if kind.trim() == "site" => {
                    super::ports::parse_site_scope_id(id).is_some()
                }
                _ => false,
            }
    })
}

fn add_wasm_indexer_component(
    state: &ApiState,
    host: &PluginHost,
    mut candidate: RegistryCandidate,
    plugin: &PluginRecord,
    version: &PluginVersionMetadata,
    comp: &plugin_package::ComponentManifest,
    plugin_gen: u64,
    trust: plugin_api::PluginTrust,
) -> Result<RegistryCandidate, String> {
    let compiled = state
        .plugins
        .cache()
        .get_or_load_indexer(state, host, plugin, &comp.entry)
        .map_err(|e| e.to_string())?;
    for idx in &comp.indexers {
        let capabilities = parse_provider_capabilities(&idx.capabilities)?;
        let auth_types = infer_auth_types(&comp.permissions, version.credential_bound);
        let descriptor = ProviderDescriptor {
            id: format!("plugin:{}/{}", plugin.plugin_id, idx.id),
            name: idx.name.clone(),
            kind: idx.kind.clone(),
            source: ProviderSource::PluginWasm {
                plugin_id: plugin.plugin_id.clone(),
                component_id: comp.id.clone(),
                version: version.version.clone(),
                plugin_generation: plugin_gen,
            },
            auth_types,
            capabilities,
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog: false,
        };
        let provider = WasmIndexerProvider::new(
            host.clone(),
            compiled.clone(),
            WasmIndexerConfig {
                plugin_id: plugin.plugin_id.clone(),
                provider_id: idx.id.clone(),
                descriptor,
                trust,
                rate_limiter: Some(state.indexer.rate_limiter().clone()),
            },
        )
        .map_err(|e| e.to_string())?;
        candidate = candidate
            .add_indexer_provider(Arc::new(provider))
            .map_err(|e| e.to_string())?;
    }
    Ok(candidate)
}

fn add_wasm_maintenance_component(
    state: &ApiState,
    host: &PluginHost,
    mut candidate: RegistryCandidate,
    plugin: &PluginRecord,
    version: &PluginVersionMetadata,
    comp: &plugin_package::ComponentManifest,
    plugin_gen: u64,
    trust: plugin_api::PluginTrust,
) -> Result<RegistryCandidate, String> {
    let compiled = state
        .plugins
        .cache()
        .get_or_load_maintenance(state, host, plugin, &comp.entry)
        .map_err(|e| e.to_string())?;
    for maint in &comp.maintenance {
        let actions = parse_maintenance_actions(&maint.actions)?;
        let descriptor = MaintenanceDescriptor {
            id: format!("plugin:{}/{}", plugin.plugin_id, maint.id),
            source: ProviderSource::PluginWasm {
                plugin_id: plugin.plugin_id.clone(),
                component_id: comp.id.clone(),
                version: version.version.clone(),
                plugin_generation: plugin_gen,
            },
            actions,
            availability: ProviderAvailability::Enabled,
        };
        let provider = WasmMaintenanceProvider::new(
            host.clone(),
            compiled.clone(),
            WasmMaintenanceConfig {
                plugin_id: plugin.plugin_id.clone(),
                provider_id: maint.id.clone(),
                descriptor,
                trust,
                rate_limiter: Some(state.indexer.rate_limiter().clone()),
            },
        );
        candidate = candidate
            .add_maintenance_provider(Arc::new(provider))
            .map_err(|e| e.to_string())?;
    }
    Ok(candidate)
}

pub(crate) fn validate_enabling_plugin_providers(
    state: &ApiState,
    plugin: &PluginRecord,
    version: &PluginVersionMetadata,
) -> Result<(), String> {
    if version.manifest_json == "{}" {
        return Ok(());
    }
    let manifest: PluginManifest = serde_json::from_str(&version.manifest_json)
        .map_err(|e| format!("corrupt manifest_json: {e}"))?;
    if !manifest_contributes_providers(&manifest) {
        return Ok(());
    }
    let host = state
        .plugins
        .wasm_host()
        .map_err(|e| e.to_string())?
        .clone();
    let mut candidate = RegistryCandidate::from_builtin()
        .map_err(|e| e.to_string())?
        .with_overlay_dir(state.indexer_overlay_dir.as_deref())
        .map_err(|e| e.to_string())?;
    let enabled = load_enabled_plugins(&state.store).map_err(|e| e.to_string())?;
    for (p, v) in enabled {
        if p.plugin_id == plugin.plugin_id {
            continue;
        }
        candidate = add_plugin_providers(state, &host, candidate, &p, &v)?;
    }
    let _ = add_plugin_providers(state, &host, candidate, plugin, version)?;
    Ok(())
}

fn snapshot_has_stale_db_provider(
    store: &Store,
    snapshot: &indexer::RegistrySnapshot,
    enabled: &[(PluginRecord, PluginVersionMetadata)],
) -> Result<bool, PluginServiceError> {
    let enabled_ids: std::collections::HashSet<&str> =
        enabled.iter().map(|(p, _)| p.plugin_id.as_str()).collect();
    let plugin_ids = snapshot
        .indexer_descriptors()
        .filter_map(|d| d.source.plugin_id())
        .chain(
            snapshot
                .maintenance_descriptors()
                .filter_map(|d| d.source.plugin_id()),
        );
    for pid in plugin_ids {
        if !enabled_ids.contains(pid) && store.get_plugin(pid)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn sync_registry_snapshot(state: &ApiState, force: bool) -> Result<(), PluginServiceError> {
    let enabled = load_enabled_plugins(&state.store)?;
    let has_provider_plugins = enabled.iter().any(|(_, v)| {
        v.manifest_json != "{}"
            && serde_json::from_str::<PluginManifest>(&v.manifest_json)
                .is_ok_and(|m| manifest_contributes_providers(&m))
    });
    let has_non_builtin_providers = enabled.iter().any(|(_, v)| {
        v.signature_status != "builtin"
            && v.manifest_json != "{}"
            && serde_json::from_str::<PluginManifest>(&v.manifest_json)
                .is_ok_and(|m| manifest_contributes_providers(&m))
    });
    let current_snap = state.indexer.snapshot();
    let snapshot_builtin_only = current_snap
        .indexer_descriptors()
        .all(|d| d.source.is_builtin())
        && current_snap
            .maintenance_descriptors()
            .all(|d| d.source.is_builtin());
    let has_stale_db_provider =
        snapshot_has_stale_db_provider(&state.store, &current_snap, &enabled)?;
    if !force
        && state.indexer_overlay_dir.is_none()
        && !has_non_builtin_providers
        && !has_stale_db_provider
        && !(has_provider_plugins && snapshot_builtin_only)
    {
        return Ok(());
    }

    let mut candidate = RegistryCandidate::from_builtin()
        .map_err(|e| PluginServiceError::conflict("plugin.registry_error", e.to_string()))?
        .with_overlay_dir(state.indexer_overlay_dir.as_deref())
        .map_err(|e| PluginServiceError::conflict("plugin.registry_error", e.to_string()))?;

    if has_provider_plugins {
        let host = state
            .plugins
            .wasm_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();
        for (p, v) in &enabled {
            candidate = add_plugin_providers(state, &host, candidate, p, v)
                .map_err(|e| PluginServiceError::conflict("plugin.registry_collision", e))?;
        }
    }

    state
        .indexer
        .publish(candidate)
        .map_err(|e| PluginServiceError::conflict("plugin.registry_collision", e.to_string()))?;
    Ok(())
}

pub(crate) fn sync_registry_snapshot_on_startup(state: &ApiState) {
    if let Err(err) = sync_registry_snapshot(state, false) {
        tracing::error!(error = %err, "Failed to rebuild plugin RegistrySnapshot on startup");
    }
}
