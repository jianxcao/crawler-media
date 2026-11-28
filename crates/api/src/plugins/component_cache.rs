use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use plugin_api::{Failure, GuestRole};
use plugin_host::{
    CompiledEndpoint, CompiledIndexer, CompiledMaintenance, CompiledPolicy, CompiledTask,
    ENDPOINT_WORLD, HOOK_WORLD, INDEXER_WORLD, Invocation, MAINTENANCE_WORLD, PluginHost,
    TASK_WORLD,
};
use plugin_package::{PluginManifest, ValidatedPackage};
use store::plugins::{
    PluginCommandCause, PluginConfig, PluginRecord, PluginSubject, PluginVersionMetadata,
};

use super::effect::PluginInvocationRole;
use super::ports::{ActiveAuth, ActiveInvocation, HostDeps};
use super::staging::safe_rel_path;
use crate::management::ApiState;

const RAW_WASM_KEY: &str = "__raw__";

#[derive(Default)]
pub(crate) struct ComponentCache {
    policies: Mutex<HashMap<(String, String, String), CompiledPolicy>>,
    tasks: Mutex<HashMap<(String, String, String), CompiledTask>>,
    endpoints: Mutex<HashMap<(String, String, String), CompiledEndpoint>>,
    indexers: Mutex<HashMap<(String, String, String), CompiledIndexer>>,
    maintenance: Mutex<HashMap<(String, String, String), CompiledMaintenance>>,
}

impl ComponentCache {
    pub(crate) fn evict_plugin(&self, plugin_id: &str) {
        self.policies.lock().retain(|k, _| k.0 != plugin_id);
        self.tasks.lock().retain(|k, _| k.0 != plugin_id);
        self.endpoints.lock().retain(|k, _| k.0 != plugin_id);
        self.indexers.lock().retain(|k, _| k.0 != plugin_id);
        self.maintenance.lock().retain(|k, _| k.0 != plugin_id);
    }

    pub(crate) fn evict_hash(&self, plugin_id: &str, package_hash: &str) {
        self.policies
            .lock()
            .retain(|k, _| k.0 != plugin_id || k.1 != package_hash);
        self.tasks
            .lock()
            .retain(|k, _| k.0 != plugin_id || k.1 != package_hash);
        self.endpoints
            .lock()
            .retain(|k, _| k.0 != plugin_id || k.1 != package_hash);
        self.indexers
            .lock()
            .retain(|k, _| k.0 != plugin_id || k.1 != package_hash);
        self.maintenance
            .lock()
            .retain(|k, _| k.0 != plugin_id || k.1 != package_hash);
    }

    pub(crate) fn get_or_load_policy_from_version(
        &self,
        data_dir: &Path,
        host: &PluginHost,
        plugin: &PluginRecord,
        version: &PluginVersionMetadata,
        entry: &str,
    ) -> Result<CompiledPolicy, Failure> {
        let key = (
            plugin.plugin_id.clone(),
            plugin.package_hash.clone(),
            entry.to_string(),
        );
        if let Some(found) = self.policies.lock().get(&key).cloned() {
            return Ok(found);
        }
        let bytes = read_component_entry_bytes(data_dir, version, entry)
            .map_err(Failure::InvalidComponent)?;
        let compiled = host.load(&bytes, HOOK_WORLD)?;
        Ok(self.policies.lock().entry(key).or_insert(compiled).clone())
    }

    pub(crate) fn get_or_load_indexer(
        &self,
        state: &ApiState,
        host: &PluginHost,
        plugin: &PluginRecord,
        entry: &str,
    ) -> Result<CompiledIndexer, Failure> {
        let key = (
            plugin.plugin_id.clone(),
            plugin.package_hash.clone(),
            entry.to_string(),
        );
        if let Some(found) = self.indexers.lock().get(&key).cloned() {
            return Ok(found);
        }
        let bytes = load_component_bytes_from_store(&state.store, plugin, entry)?;
        let compiled = host.load_indexer(&bytes, INDEXER_WORLD)?;
        Ok(self.indexers.lock().entry(key).or_insert(compiled).clone())
    }

    pub(crate) fn get_or_load_maintenance(
        &self,
        state: &ApiState,
        host: &PluginHost,
        plugin: &PluginRecord,
        entry: &str,
    ) -> Result<CompiledMaintenance, Failure> {
        let key = (
            plugin.plugin_id.clone(),
            plugin.package_hash.clone(),
            entry.to_string(),
        );
        if let Some(found) = self.maintenance.lock().get(&key).cloned() {
            return Ok(found);
        }
        let bytes = load_component_bytes_from_store(&state.store, plugin, entry)?;
        let compiled = host.load_maintenance(&bytes, MAINTENANCE_WORLD)?;
        Ok(self
            .maintenance
            .lock()
            .entry(key)
            .or_insert(compiled)
            .clone())
    }

    pub(crate) fn get_or_load_task(
        &self,
        state: &ApiState,
        host: &PluginHost,
        plugin: &PluginRecord,
        entry: Option<&str>,
    ) -> Result<CompiledTask, String> {
        let entry_key = entry.unwrap_or(RAW_WASM_KEY);
        let key = (
            plugin.plugin_id.clone(),
            plugin.package_hash.clone(),
            entry_key.to_string(),
        );
        if let Some(found) = self.tasks.lock().get(&key).cloned() {
            return Ok(found);
        }
        let bytes = match entry {
            Some(ent) => load_component_bytes_from_store(&state.store, plugin, ent)
                .map_err(|e| e.to_string())?,
            None => read_raw_or_world_bytes(state, plugin, TASK_WORLD)?,
        };
        let compiled = host
            .load_task(&bytes, TASK_WORLD)
            .map_err(|e| e.to_string())?;
        let mut cache = self.tasks.lock();
        Ok(cache.entry(key).or_insert(compiled).clone())
    }

    pub(crate) fn get_or_load_endpoint(
        &self,
        state: &ApiState,
        host: &PluginHost,
        plugin: &PluginRecord,
        entry: Option<&str>,
    ) -> Result<CompiledEndpoint, Failure> {
        let entry_key = entry.unwrap_or(RAW_WASM_KEY);
        let key = (
            plugin.plugin_id.clone(),
            plugin.package_hash.clone(),
            entry_key.to_string(),
        );
        if let Some(found) = self.endpoints.lock().get(&key).cloned() {
            return Ok(found);
        }
        let bytes = match entry {
            Some(ent) => load_component_bytes_from_store(&state.store, plugin, ent)?,
            None => read_raw_or_world_bytes(state, plugin, ENDPOINT_WORLD)
                .map_err(Failure::InvalidComponent)?,
        };
        let compiled = host.load_endpoint(&bytes, ENDPOINT_WORLD)?;
        let mut cache = self.endpoints.lock();
        Ok(cache.entry(key).or_insert(compiled).clone())
    }
}

fn load_component_bytes_from_store(
    store: &store::Store,
    plugin: &PluginRecord,
    entry: &str,
) -> Result<Vec<u8>, Failure> {
    let version = store
        .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        .map_err(|e| Failure::InvalidComponent(e.to_string()))?
        .ok_or_else(|| {
            Failure::InvalidComponent(format!(
                "missing plugin version metadata for {}@{}",
                plugin.plugin_id, plugin.package_hash
            ))
        })?;
    read_component_entry_bytes(store.data_dir(), &version, entry).map_err(Failure::InvalidComponent)
}

pub(crate) fn resolve_artifact_path(data_dir: &Path, artifact_path: &str) -> PathBuf {
    let artifact = Path::new(artifact_path);
    if artifact.is_absolute() {
        artifact.to_path_buf()
    } else {
        data_dir.join(artifact)
    }
}

pub(crate) fn read_component_entry_bytes(
    data_dir: &Path,
    version: &PluginVersionMetadata,
    entry: &str,
) -> Result<Vec<u8>, String> {
    let base = resolve_artifact_path(data_dir, &version.artifact_path);
    if base.is_dir() {
        let full = safe_rel_path(&base, entry).map_err(|e| e.to_string())?;
        return std::fs::read(&full).map_err(|e| {
            tracing::warn!(
                plugin_id = %version.plugin_id,
                package_hash = %version.package_hash,
                component_path = %full.display(),
                error = %e,
                "failed to read plugin component entry"
            );
            format!("failed to read plugin component `{entry}`")
        });
    }
    std::fs::read(&base).map_err(|e| {
        tracing::warn!(
            plugin_id = %version.plugin_id,
            package_hash = %version.package_hash,
            component_path = %base.display(),
            error = %e,
            "failed to read plugin component"
        );
        format!("failed to read plugin component `{entry}`")
    })
}

fn read_raw_or_world_bytes(
    state: &ApiState,
    plugin: &PluginRecord,
    world: &str,
) -> Result<Vec<u8>, String> {
    let version = state
        .store
        .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "missing plugin version metadata for {}@{}",
                plugin.plugin_id, plugin.package_hash
            )
        })?;
    let base = resolve_artifact_path(state.store.data_dir(), &version.artifact_path);
    if base.is_dir() {
        let manifest: PluginManifest = serde_json::from_str(&version.manifest_json)
            .map_err(|e| format!("invalid plugin manifest_json: {e}"))?;
        let comp = manifest
            .components
            .iter()
            .find(|c| c.world == world)
            .ok_or_else(|| {
                format!(
                    "plugin {} has no component for world {world}",
                    plugin.plugin_id
                )
            })?;
        return read_component_entry_bytes(state.store.data_dir(), &version, &comp.entry);
    }
    std::fs::read(&base).map_err(|e| {
        tracing::warn!(
            plugin_id = %plugin.plugin_id,
            package_hash = %plugin.package_hash,
            component_path = %base.display(),
            error = %e,
            "failed to read plugin component"
        );
        "failed to read plugin component".to_string()
    })
}

pub(crate) fn compile_validated_package_components(
    host: &PluginHost,
    pkg: &ValidatedPackage,
) -> Result<(), Failure> {
    for comp in &pkg.manifest.components {
        let bytes = pkg.component_bytes.get(&comp.entry).ok_or_else(|| {
            Failure::InvalidComponent(format!("missing component `{}`", comp.entry))
        })?;
        compile_component_for_world(host, bytes, &comp.world)?;
    }
    Ok(())
}

fn compile_component_for_world(
    host: &PluginHost,
    bytes: &[u8],
    world: &str,
) -> Result<(), Failure> {
    match world {
        HOOK_WORLD => {
            let _ = host.load(bytes, HOOK_WORLD)?;
        }
        TASK_WORLD => {
            let _ = host.load_task(bytes, TASK_WORLD)?;
        }
        ENDPOINT_WORLD => {
            let _ = host.load_endpoint(bytes, ENDPOINT_WORLD)?;
        }
        INDEXER_WORLD => {
            let _ = host.load_indexer(bytes, INDEXER_WORLD)?;
        }
        MAINTENANCE_WORLD => {
            let _ = host.load_maintenance(bytes, MAINTENANCE_WORLD)?;
        }
        other => {
            return Err(Failure::InvalidComponent(format!(
                "unsupported component world: {other}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_installed_components(
    state: &ApiState,
    validation_host: &PluginHost,
    plugin_id: &str,
    version: &PluginVersionMetadata,
    actor_user_id: domain::UserId,
    config: Option<PluginConfig>,
) -> Result<(), Failure> {
    if version.manifest_json == "{}" {
        return Ok(());
    }
    let data_dir = state.store.data_dir();
    let manifest: PluginManifest = serde_json::from_str(&version.manifest_json)
        .map_err(|e| Failure::InvalidComponent(format!("corrupt manifest_json: {e}")))?;
    let state_schema = version
        .state_schema
        .parse::<u32>()
        .map_err(|e| Failure::InvalidComponent(format!("invalid state_schema: {e}")))?;
    for comp in &manifest.components {
        if version.signature_status == "builtin" && comp.entry == "builtin" {
            continue;
        }
        let bytes = read_component_entry_bytes(data_dir, version, &comp.entry)
            .map_err(Failure::InvalidComponent)?;
        let mut inv = Invocation::new(plugin_id.to_string(), Duration::from_secs(5));
        inv.role = GuestRole::Lifecycle;
        let _guard = state.plugins.ports().register(
            inv.id().to_string(),
            ActiveInvocation {
                deps: HostDeps::from_state(state),
                auth: ActiveAuth::Lifecycle {
                    plugin_id: plugin_id.to_string(),
                    config: config.clone(),
                },
                subject: PluginSubject::User(actor_user_id),
                cause: PluginCommandCause::root(format!("lifecycle-validate:{plugin_id}")),
                role: PluginInvocationRole::Lifecycle,
                manifest_http: Arc::new(Vec::new()),
                max_response_bytes: None,
            },
        );
        validate_one_component(
            validation_host,
            &comp.world,
            &bytes,
            inv,
            &version.config_schema_json,
            state_schema,
        )?;
    }
    Ok(())
}

fn validate_one_component(
    validation_host: &PluginHost,
    world: &str,
    bytes: &[u8],
    inv: Invocation,
    config_schema_json: &str,
    state_schema: u32,
) -> Result<(), Failure> {
    match world {
        HOOK_WORLD => {
            let c = validation_host.load(bytes, HOOK_WORLD)?;
            validation_host.validate_policy(&c, inv, config_schema_json, state_schema)
        }
        TASK_WORLD => {
            let c = validation_host.load_task(bytes, TASK_WORLD)?;
            validation_host.validate_task(&c, inv, config_schema_json, state_schema)
        }
        ENDPOINT_WORLD => {
            let c = validation_host.load_endpoint(bytes, ENDPOINT_WORLD)?;
            validation_host.validate_endpoint(&c, inv, config_schema_json, state_schema)
        }
        INDEXER_WORLD => {
            let c = validation_host.load_indexer(bytes, INDEXER_WORLD)?;
            validation_host.validate_indexer(&c, inv, config_schema_json, state_schema)
        }
        MAINTENANCE_WORLD => {
            let c = validation_host.load_maintenance(bytes, MAINTENANCE_WORLD)?;
            validation_host.validate_maintenance(&c, inv, config_schema_json, state_schema)
        }
        other => Err(Failure::InvalidComponent(format!(
            "unsupported component world: {other}"
        ))),
    }
}
