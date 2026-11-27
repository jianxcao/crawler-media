use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;

pub(crate) use super::component_cache::{
    ComponentCache, compile_validated_package_components, resolve_artifact_path,
    validate_installed_components,
};
pub(crate) use super::in_flight::{InFlightRegistry, OwnedInFlightGuard};
use plugin_api::{
    EndpointRequest, EndpointResponse, ErrorCode, Failure, GuestRole, PluginTrust, TaskInput,
};
use plugin_host::{ENDPOINT_WORLD, Invocation, TASK_WORLD};
use plugin_package::PluginManifest;
use store::plugins::{
    PluginCommandCause, PluginLifecycle, PluginRecord, PluginStateAuthority, PluginSubject,
    PluginVersionMetadata,
};

use super::effect::{PluginInvocationRole, PluginTaskInvocation, PluginTaskRunner};
use super::ports::{ActiveAuth, ActiveInvocation, HostDeps, StoreHostPorts, api_err};
use crate::management::ApiState;

pub(crate) fn resolve_plugin_trust(
    plugin_id: &str,
    signature_status: &str,
    admin_trusted: &HashSet<String>,
) -> PluginTrust {
    if signature_status == "builtin" {
        PluginTrust::BuiltIn
    } else if admin_trusted.contains(plugin_id) {
        PluginTrust::AdminTrusted
    } else {
        PluginTrust::Untrusted
    }
}

fn build_invocation(
    plugin: &PluginRecord,
    authority: &PluginStateAuthority,
    subject: &PluginSubject,
    cause: &PluginCommandCause,
    budget: Duration,
) -> Result<Invocation, String> {
    let mut inv = Invocation::new(plugin.plugin_id.clone(), budget);
    inv.generation = u64::try_from(authority.generation()).unwrap_or(0);
    inv.config_revision = u64::try_from(plugin.config_revision).unwrap_or(0);
    inv.subject = serde_json::to_string(subject).map_err(|e| e.to_string())?;
    inv.site_id = authority.site_id().map(|id| id.to_string());
    inv.cause_id = cause.cause_id.clone();
    Ok(inv)
}

fn resolve_task_credential_epoch(
    state: &ApiState,
    authority: &PluginStateAuthority,
) -> Result<Option<String>, String> {
    state
        .store
        .validate_plugin_state_authority(authority)
        .map_err(|e| e.to_string())?;
    let Some(sid) = authority.site_id() else {
        return Ok(None);
    };
    Ok(state
        .store
        .observe_site(sid)
        .map_err(|e| e.to_string())?
        .map(|obs| obs.credential_epoch().storage_key().to_string()))
}

fn resolve_task_component(
    plugin_id: &str,
    version: Option<&PluginVersionMetadata>,
    task_id: &str,
) -> Result<(Option<String>, Option<HashSet<String>>), String> {
    let Some(ver) = version.filter(|v| v.manifest_json != "{}") else {
        return Ok((None, None));
    };
    let manifest: PluginManifest = serde_json::from_str(&ver.manifest_json)
        .map_err(|e| format!("invalid plugin manifest_json: {e}"))?;
    if manifest.components.is_empty() {
        return Ok((None, None));
    }
    let comp = manifest
        .components
        .iter()
        .filter(|c| c.world == TASK_WORLD)
        .find(|c| c.tasks.iter().any(|t| t.id == task_id))
        .ok_or_else(|| format!("plugin {plugin_id} task `{task_id}` not declared in manifest"))?;
    Ok((
        Some(comp.entry.clone()),
        Some(comp.permissions.iter().cloned().collect()),
    ))
}

fn resolve_endpoint_component(
    _plugin_id: &str,
    version: Option<&PluginVersionMetadata>,
    request: &EndpointRequest,
) -> Result<(Option<String>, Option<HashSet<String>>), Failure> {
    let Some(ver) = version.filter(|v| v.manifest_json != "{}") else {
        return Ok((None, None));
    };
    let manifest: PluginManifest = serde_json::from_str(&ver.manifest_json)
        .map_err(|e| Failure::InvalidComponent(format!("invalid plugin manifest_json: {e}")))?;
    let path_clean = request.path.split('?').next().unwrap_or(&request.path);
    let ep_comps: Vec<_> = manifest
        .components
        .iter()
        .filter(|c| c.world == ENDPOINT_WORLD)
        .collect();
    let by_path: Vec<_> = ep_comps
        .iter()
        .copied()
        .filter(|c| {
            c.endpoints.iter().any(|ep| {
                ep.method.eq_ignore_ascii_case(&request.method)
                    && (ep.path == request.path || ep.path == path_clean)
            })
        })
        .collect();
    let matched = if !by_path.is_empty() {
        by_path
    } else {
        ep_comps
            .into_iter()
            .filter(|c| {
                c.endpoints.iter().any(|ep| {
                    ep.method.eq_ignore_ascii_case(&request.method)
                        && (format!("/{}", ep.id) == request.path
                            || format!("/{}", ep.id) == path_clean
                            || ep.id == request.path
                            || ep.id == path_clean)
                })
            })
            .collect()
    };
    let comp = match matched.as_slice() {
        [only] => *only,
        [] => {
            return Err(Failure::Guest(api_err(
                ErrorCode::NotFound,
                "endpoint not declared in manifest",
            )));
        }
        _ => {
            return Err(Failure::Guest(api_err(
                ErrorCode::Conflict,
                "ambiguous endpoint declaration in manifest",
            )));
        }
    };
    Ok((
        Some(comp.entry.clone()),
        Some(comp.permissions.iter().cloned().collect()),
    ))
}

fn register_task_invocation(
    state: &ApiState,
    invocation: &PluginTaskInvocation,
    guest_inv: &plugin_host::Invocation,
    subject: PluginSubject,
    comp_perms: Option<&HashSet<String>>,
) -> Result<super::ports::InvocationGuard, String> {
    let mut grants = state
        .store
        .get_plugin_grants(&invocation.plugin.plugin_id)
        .map_err(|e| e.to_string())?;
    if let Some(perms) = comp_perms {
        grants.retain(|g| perms.contains(&g.capability));
    }
    let (manifest_http, max_response_bytes) = StoreHostPorts::load_manifest_http(
        &state.store,
        &invocation.plugin.plugin_id,
        &invocation.plugin.package_hash,
    );
    let deps = HostDeps::from_state(state);
    let invocation_site = StoreHostPorts::try_resolve_optional_site(&deps, guest_inv)
        .map_err(|e| Failure::Guest(e).to_string())?;
    Ok(state.plugins.ports().register(
        guest_inv.id().to_string(),
        ActiveInvocation {
            deps,
            auth: ActiveAuth::Runtime {
                authority: invocation.authority.clone(),
                grants,
                invocation_site,
            },
            subject,
            cause: invocation.cause.clone(),
            role: PluginInvocationRole::Task,
            manifest_http,
            max_response_bytes,
        },
    ))
}

pub(crate) struct WasmTaskRunner;

impl PluginTaskRunner for WasmTaskRunner {
    fn run_task(&self, state: &ApiState, invocation: &PluginTaskInvocation) -> Result<(), String> {
        state
            .store
            .validate_plugin_state_authority(&invocation.authority)
            .map_err(|e| e.to_string())?;
        let version = state
            .store
            .get_plugin_version(
                &invocation.plugin.plugin_id,
                &invocation.plugin.package_hash,
            )
            .map_err(|e| e.to_string())?;
        let (comp_entry, comp_perms) = resolve_task_component(
            &invocation.plugin.plugin_id,
            version.as_ref(),
            &invocation.binding.task_id,
        )?;
        let host = state.plugins.wasm_host().map_err(|e| e.to_string())?;
        let compiled = state.plugins.cache().get_or_load_task(
            state,
            host,
            &invocation.plugin,
            comp_entry.as_deref(),
        )?;
        let effective_input = invocation
            .input_override
            .as_ref()
            .unwrap_or(&invocation.binding.definition.public_config);
        let validated_input = serde_json::to_string(effective_input).map_err(|e| e.to_string())?;
        let subject = PluginSubject::Task {
            plugin_id: invocation.plugin.plugin_id.clone(),
            task_id: invocation.binding.task_id.clone(),
        };
        let trust = resolve_plugin_trust(
            &invocation.plugin.plugin_id,
            version.as_ref().map_or("", |v| v.signature_status.as_str()),
            &state.admin_trusted_plugins,
        );
        let mut guest_inv = build_invocation(
            &invocation.plugin,
            &invocation.authority,
            &subject,
            &invocation.cause,
            Duration::from_secs(60),
        )?;
        guest_inv.trust = trust;
        guest_inv.role = GuestRole::Task;
        guest_inv.credential_epoch = resolve_task_credential_epoch(state, &invocation.authority)?;
        if guest_inv.site_id.is_none() {
            guest_inv.site_id = invocation
                .site
                .as_ref()
                .map(|obs| obs.site().id.to_string());
        }
        let _flight = state
            .plugins
            .in_flight()
            .try_enter(
                &invocation.plugin.plugin_id,
                invocation.authority.generation(),
                &guest_inv,
            )
            .map_err(|e| e.to_string())?;
        let _guard =
            register_task_invocation(state, invocation, &guest_inv, subject, comp_perms.as_ref())?;
        let _task_state_guard = state
            .plugins
            .ports()
            .register_task_state(guest_inv.id().to_string(), state.clone());
        let input = TaskInput {
            task_id: invocation.binding.task_id.clone(),
            validated_input,
            job_id: invocation.job_id.to_string(),
            attempt: invocation.attempt,
        };
        let outcome = match host.invoke_task(&compiled, guest_inv, input) {
            Ok(outcome) => outcome,
            Err(err) => {
                state
                    .plugins
                    .in_flight()
                    .record_error(&invocation.plugin.plugin_id);
                return Err(err.to_string());
            }
        };
        tracing::info!(
            plugin_id = %invocation.plugin.plugin_id,
            task_id = %invocation.binding.task_id,
            progress = outcome.progress,
            partial = outcome.partial,
            operations = outcome.operation_ids.len(),
            "Plugin task execution completed"
        );
        Ok(())
    }
}

fn parse_endpoint_query_site(raw_q: &str) -> Result<domain::SiteId, Failure> {
    domain::SiteId::from_str(super::ports::strip_site_prefix(raw_q)).map_err(|_| {
        Failure::Guest(api_err(
            ErrorCode::InvalidArgument,
            "invalid endpoint site_id",
        ))
    })
}

fn observe_required_endpoint_site(
    state: &ApiState,
    sid: domain::SiteId,
) -> Result<store::SiteObservation, Failure> {
    let obs = state
        .store
        .observe_site(sid)
        .map_err(super::ports::map_store_error)
        .map_err(Failure::Guest)?
        .ok_or_else(|| Failure::Guest(api_err(ErrorCode::NotFound, "bound site not found")))?;
    if !obs.site().enabled {
        return Err(Failure::Guest(api_err(
            ErrorCode::StaleGeneration,
            "bound site is disabled",
        )));
    }
    Ok(obs)
}

fn resolve_endpoint_authority_and_epoch(
    state: &ApiState,
    plugin: &PluginRecord,
    version: Option<&PluginVersionMetadata>,
    authority: PluginStateAuthority,
    request: &EndpointRequest,
) -> Result<(PluginStateAuthority, Option<String>, Option<String>), Failure> {
    let query_site = request
        .query
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("site_id"))
        .map(|h| h.value.trim())
        .filter(|s| !s.is_empty());
    let credential_bound = version.is_some_and(|v| v.credential_bound);
    match (credential_bound, authority.site_id(), query_site) {
        (true, Some(auth_sid), maybe_q) => {
            if let Some(raw_q) = maybe_q
                && parse_endpoint_query_site(raw_q)? != auth_sid
            {
                return Err(Failure::Guest(api_err(
                    ErrorCode::PermissionDenied,
                    "endpoint site_id does not match bound authority",
                )));
            }
            state
                .store
                .validate_plugin_state_authority(&authority)
                .map_err(super::ports::map_store_error)
                .map_err(Failure::Guest)?;
            let obs = observe_required_endpoint_site(state, auth_sid)?;
            Ok((
                authority,
                Some(obs.credential_epoch().storage_key().to_string()),
                Some(auth_sid.to_string()),
            ))
        }
        (true, None, Some(raw_q)) => {
            let q_sid = parse_endpoint_query_site(raw_q)?;
            let obs = observe_required_endpoint_site(state, q_sid)?;
            let site_auth = state
                .store
                .issue_plugin_state_authority(plugin, Some(&obs))
                .map_err(super::ports::map_store_error)
                .map_err(Failure::Guest)?;
            Ok((
                site_auth,
                Some(obs.credential_epoch().storage_key().to_string()),
                Some(q_sid.to_string()),
            ))
        }
        (true, None, None) => Err(Failure::Guest(api_err(
            ErrorCode::PermissionDenied,
            "credential-bound endpoint requires site_id",
        ))),
        (false, Some(auth_sid), maybe_q) => {
            if let Some(raw_q) = maybe_q
                && parse_endpoint_query_site(raw_q)? != auth_sid
            {
                return Err(Failure::Guest(api_err(
                    ErrorCode::PermissionDenied,
                    "endpoint site_id does not match bound authority",
                )));
            }
            state
                .store
                .validate_plugin_state_authority(&authority)
                .map_err(super::ports::map_store_error)
                .map_err(Failure::Guest)?;
            let obs = observe_required_endpoint_site(state, auth_sid)?;
            Ok((
                authority,
                Some(obs.credential_epoch().storage_key().to_string()),
                Some(auth_sid.to_string()),
            ))
        }
        (false, None, maybe_q) => {
            state
                .store
                .validate_plugin_state_authority(&authority)
                .map_err(super::ports::map_store_error)
                .map_err(Failure::Guest)?;
            let query_sid = match maybe_q {
                Some(raw_q) => {
                    let q_sid = parse_endpoint_query_site(raw_q)?;
                    let _ = observe_required_endpoint_site(state, q_sid)?;
                    Some(q_sid.to_string())
                }
                None => None,
            };
            Ok((authority, None, query_sid))
        }
    }
}

pub(super) fn invoke_endpoint(
    service: &super::PluginService,
    state: &ApiState,
    plugin: &PluginRecord,
    authority: PluginStateAuthority,
    subject: PluginSubject,
    cause: PluginCommandCause,
    request: EndpointRequest,
) -> Result<EndpointResponse, Failure> {
    if plugin.lifecycle != PluginLifecycle::Enabled
        || !plugin.desired_enabled
        || plugin.generation != authority.generation()
        || plugin.plugin_id != authority.plugin_id()
    {
        return Err(Failure::Guest(api_err(
            ErrorCode::StaleGeneration,
            "stale plugin generation",
        )));
    }
    let version = state
        .store
        .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        .map_err(|e| Failure::Trap(e.to_string()))?;
    let (comp_entry, comp_perms) =
        resolve_endpoint_component(&plugin.plugin_id, version.as_ref(), &request)?;
    let host = service.wasm_host()?;
    let compiled =
        service
            .cache()
            .get_or_load_endpoint(state, host, plugin, comp_entry.as_deref())?;
    let (authority, bound_epoch, query_site_id) =
        resolve_endpoint_authority_and_epoch(state, plugin, version.as_ref(), authority, &request)?;
    let trust = resolve_plugin_trust(
        &plugin.plugin_id,
        version.as_ref().map_or("", |v| v.signature_status.as_str()),
        &state.admin_trusted_plugins,
    );
    let mut guest_inv = build_invocation(
        plugin,
        &authority,
        &subject,
        &cause,
        Duration::from_secs(15),
    )
    .map_err(Failure::InvalidInput)?;
    guest_inv.credential_epoch = bound_epoch;
    if guest_inv.site_id.is_none() {
        guest_inv.site_id = query_site_id;
    }
    guest_inv.trust = trust;
    guest_inv.role = GuestRole::Endpoint;
    let _flight =
        service
            .in_flight()
            .try_enter(&plugin.plugin_id, authority.generation(), &guest_inv)?;
    let mut grants = state
        .store
        .get_plugin_grants(&plugin.plugin_id)
        .map_err(|e| Failure::Trap(e.to_string()))?;
    if let Some(perms) = &comp_perms {
        grants.retain(|g| perms.contains(&g.capability));
    }
    let (manifest_http, max_response_bytes) =
        StoreHostPorts::load_manifest_http(&state.store, &plugin.plugin_id, &plugin.package_hash);
    let deps = HostDeps::from_state(state);
    let invocation_site =
        StoreHostPorts::try_resolve_optional_site(&deps, &guest_inv).map_err(Failure::Guest)?;
    let ep_role = if request.method.eq_ignore_ascii_case("GET") {
        PluginInvocationRole::ReadOnlyEndpoint
    } else {
        PluginInvocationRole::Endpoint
    };
    let _guard = service.ports().register(
        guest_inv.id().to_string(),
        ActiveInvocation {
            deps,
            auth: ActiveAuth::Runtime {
                authority,
                grants,
                invocation_site,
            },
            subject,
            cause,
            role: ep_role,
            manifest_http,
            max_response_bytes,
        },
    );
    let res = host.invoke_endpoint(&compiled, guest_inv, request);
    if res.is_err() {
        service.in_flight().record_error(&plugin.plugin_id);
    }
    res
}
