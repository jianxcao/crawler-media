use std::str::FromStr;

use domain::{JobDefId, PluginTaskBindingId, SiteId};
use store::StoreError;
use store::plugins::{PluginCommandCause, PluginError, PluginLifecycle, PluginTaskBinding};

use super::effect::PluginTaskInvocation;
use super::errors::PluginServiceError;
use crate::management::ApiState;

fn task_spec(binding_id: PluginTaskBindingId, binding: &PluginTaskBinding) -> jobs::PluginTaskSpec {
    let schedule = binding.definition.interval_secs.and_then(|secs| {
        u64::try_from(secs)
            .ok()
            .filter(|secs| *secs > 0)
            .map(|secs| jobs::Schedule::Interval { secs })
    });
    jobs::PluginTaskSpec {
        binding_id,
        generation: binding.generation,
        definition_revision: binding.definition_revision,
        name: format!("plugin:{}:{}", binding.plugin_id, binding.task_id),
        enabled: binding.definition.enabled,
        schedule,
        payload: serde_json::json!({
            "binding_id": binding.binding_id,
            "plugin_id": binding.plugin_id,
            "task_id": binding.task_id,
        })
        .to_string(),
        timeout_secs: Some(60),
        concurrency_key: Some(format!("plugin_task:{}", binding.binding_id)),
    }
}

pub(super) fn is_plugin_active_for_binding(
    state: &ApiState,
    binding: &PluginTaskBinding,
) -> Result<bool, StoreError> {
    let plugin = state.store.get_plugin(&binding.plugin_id)?;
    Ok(plugin.is_some_and(|p| {
        p.lifecycle == PluginLifecycle::Enabled
            && p.desired_enabled
            && p.generation == binding.generation
    }))
}

pub(super) fn sync_task_binding(
    state: &ApiState,
    binding: &PluginTaskBinding,
) -> Result<(), PluginServiceError> {
    sync_task_binding_with_handles(&state.store, &state.jobs, binding)
}

pub(super) fn sync_task_binding_with_handles(
    store: &store::Store,
    jobs: &crate::JobsWriter,
    binding: &PluginTaskBinding,
) -> Result<(), PluginServiceError> {
    let plugin = store.get_plugin(&binding.plugin_id)?;
    let plugin_active = plugin.is_some_and(|p| {
        p.lifecycle == PluginLifecycle::Enabled
            && p.desired_enabled
            && p.generation == binding.generation
    });
    let Some(current) = store.get_plugin_task_binding_by_id(binding.binding_id)? else {
        return Ok(());
    };
    if current.generation != binding.generation
        || current.definition_revision != binding.definition_revision
        || !current.projection_pending()
    {
        return Ok(());
    }
    let binding_id = PluginTaskBindingId::from_uuid(current.binding_id);
    let def_id = {
        let reservation = jobs.reserve()?;
        let queue = reservation.acquire()?;
        let proj = if plugin_active {
            let spec = task_spec(binding_id, &current);
            let proj = queue.project_plugin_task(&spec)?;
            if spec.enabled && spec.schedule.is_some() {
                queue.ensure_scheduled_identity(0, jobs::JobIdentity::plugin_task(binding_id))?;
            }
            proj
        } else {
            queue.close_plugin_task(binding_id, current.generation, current.definition_revision)?
        };
        proj.fence.def_id.map(|id| id.to_string())
    };
    match store.acknowledge_plugin_task_sync(
        current.binding_id,
        current.generation,
        current.definition_revision,
        def_id.as_deref(),
    ) {
        Ok(()) => Ok(()),
        Err(StoreError::Plugin(PluginError::Conflict)) => {
            tracing::debug!(
                binding_id = %current.binding_id,
                generation = current.generation,
                revision = current.definition_revision,
                "Plugin task sync changed before acknowledgment"
            );
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn disable_stale_task_def(state: &ApiState, def_id: JobDefId) {
    if let Ok(queue) = state.jobs.acquire() {
        let _ = queue.set_def_enabled_by_id(def_id, false);
    }
}

fn extract_non_empty_site_str(
    binding_id: uuid::Uuid,
    raw: Option<&serde_json::Value>,
) -> Result<Option<&str>, ()> {
    let Some(val) = raw.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(s) = val.as_str() else {
        tracing::warn!(%binding_id, "Plugin task site_id config is not a string");
        return Err(());
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed))
}

fn resolve_task_site(
    state: &ApiState,
    binding: &PluginTaskBinding,
    input_override: Option<&serde_json::Value>,
) -> Result<Option<Option<store::SiteObservation>>, String> {
    let override_site = match extract_non_empty_site_str(
        binding.binding_id,
        input_override.and_then(|v| v.get("site_id")),
    ) {
        Ok(s) => s,
        Err(()) => return Ok(None),
    };
    let cfg_site = match extract_non_empty_site_str(
        binding.binding_id,
        binding.definition.public_config.get("site_id"),
    ) {
        Ok(s) => s,
        Err(()) => return Ok(None),
    };
    let Some(site_str) = override_site.or(cfg_site) else {
        return Ok(Some(None));
    };
    let normalized = super::ports::strip_site_prefix(site_str);
    let Ok(site_id) = SiteId::from_str(normalized) else {
        tracing::warn!(binding_id = %binding.binding_id, "Plugin task site_id is invalid UUID");
        return Ok(None);
    };
    let Some(obs) = state
        .store
        .observe_site(site_id)
        .map_err(|e| e.to_string())?
    else {
        tracing::warn!(binding_id = %binding.binding_id, %site_id, "Plugin task target site no longer exists");
        return Ok(None);
    };
    if !obs.site().enabled {
        tracing::warn!(binding_id = %binding.binding_id, %site_id, "Plugin task target site is disabled");
        return Ok(None);
    }
    Ok(Some(Some(obs)))
}

pub fn run_task_job(state: &ApiState, job: &jobs::Job) -> Result<(), String> {
    let jobs::JobScope::PluginTask(binding_id) = job.scope else {
        return Err("PluginTask requires PluginTask scope".into());
    };
    let Some(binding) = state
        .store
        .get_plugin_task_binding_by_id(binding_id.as_uuid())
        .map_err(|e| e.to_string())?
    else {
        if let Some(def_id) = job.def_id {
            disable_stale_task_def(state, def_id);
        }
        return Ok(());
    };
    let Some(plugin) = state
        .store
        .get_plugin(&binding.plugin_id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if !binding.definition.enabled
        || plugin.lifecycle != PluginLifecycle::Enabled
        || !plugin.desired_enabled
        || plugin.generation != binding.generation
    {
        if let Some(def_id) = job.def_id {
            disable_stale_task_def(state, def_id);
        }
        return Ok(());
    }
    let input_override = serde_json::from_str::<serde_json::Value>(&job.payload)
        .ok()
        .and_then(|v| v.get("input").cloned());
    let Some(site_obs) = resolve_task_site(state, &binding, input_override.as_ref())? else {
        return Ok(());
    };
    let credential_bound = state
        .store
        .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        .map_err(|e| e.to_string())?
        .is_some_and(|v| v.credential_bound);
    let auth_site = if credential_bound {
        site_obs.as_ref()
    } else {
        None
    };
    let authority = match state.store.issue_plugin_state_authority(&plugin, auth_site) {
        Ok(auth) => auth,
        Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
            tracing::warn!(plugin_id = %binding.plugin_id, task_id = %binding.task_id, "Skipping plugin task under stale or denied authority");
            return Ok(());
        }
        Err(error) => return Err(error.to_string()),
    };
    let invocation = PluginTaskInvocation {
        binding,
        plugin,
        authority,
        site: site_obs,
        job_id: job.id,
        attempt: job.attempt,
        cause: PluginCommandCause::root(format!("job:{}", job.id)),
        input_override,
    };
    state.plugins.task_runner().run_task(state, &invocation)
}
