use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use domain::SiteId;
use hooks::{Gate, PluginInvoker};
use plugin_api::{Event, Failure, FilterInput, GateInput, GuestRole, HookContext, Patch};
use plugin_host::{CompiledPolicy, Invocation};
use store::StoreError;
use store::plugins::{
    PluginCommandCause, PluginError, PluginGrant, PluginGrantScope, PluginRecord,
    PluginStateAuthority, PluginSubject, PluginVersionMetadata,
};

use crate::management::ApiState;
use crate::plugins::PluginService;
use crate::plugins::effect::PluginInvocationRole;
use crate::plugins::ports::{
    ActiveAuth, ActiveInvocation, HostDeps, InvocationGuard, StoreHostPorts, api_err,
    map_store_error, parse_site_scope_id, strip_site_prefix,
};

#[derive(Clone)]
pub(crate) enum PolicyAuthority {
    Fixed {
        authority: PluginStateAuthority,
        credential_epoch: Option<String>,
    },
    PerInvocationUnbound,
    PerInvocationSite,
}

fn strip_optional_kind_prefix<'a>(raw: &'a str, kind: &str) -> &'a str {
    let trimmed = raw.trim();
    if trimmed.len() > kind.len()
        && trimmed
            .get(..kind.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(kind))
        && trimmed.as_bytes().get(kind.len()) == Some(&b':')
    {
        trimmed[kind.len() + 1..].trim()
    } else {
        trimmed
    }
}

fn scoped_object_ids_match(kind: &str, granted_id: &str, target_id: &str) -> bool {
    let g = strip_optional_kind_prefix(granted_id, kind);
    let t = strip_optional_kind_prefix(target_id, kind);
    match (uuid::Uuid::parse_str(g), uuid::Uuid::parse_str(t)) {
        (Ok(gu), Ok(tu)) => gu == tu,
        _ => !g.is_empty() && g.eq_ignore_ascii_case(t),
    }
}

pub(crate) fn grant_covers_target_scope(
    scope: &PluginGrantScope,
    site_id: Option<&str>,
    library_id: Option<&str>,
    subscribe_id: Option<&str>,
) -> bool {
    match scope {
        PluginGrantScope::Global => true,
        PluginGrantScope::Site(expected_sid) => site_id
            .and_then(parse_site_scope_id)
            .is_some_and(|sid| sid == *expected_sid),
        PluginGrantScope::Object { kind, id } => match kind.trim() {
            "site" => site_id
                .and_then(parse_site_scope_id)
                .is_some_and(|sid| parse_site_scope_id(id) == Some(sid)),
            "library" => library_id.is_some_and(|lid| scoped_object_ids_match("library", id, lid)),
            "subscribe" => {
                subscribe_id.is_some_and(|sub| scoped_object_ids_match("subscribe", id, sub))
            }
            _ => false,
        },
    }
}

pub(crate) fn hook_grants_cover_context(
    grants: &[PluginGrant],
    hook_id: &str,
    ctx: &HookContext,
) -> bool {
    cap_grants_cover_context(grants, &format!("hooks:{hook_id}"), ctx)
}

fn cap_grants_cover_context(grants: &[PluginGrant], required_cap: &str, ctx: &HookContext) -> bool {
    grants.iter().any(|g| {
        g.capability == required_cap
            && grant_covers_target_scope(
                &g.scope,
                ctx.site_id.as_deref(),
                ctx.library_id.as_deref(),
                ctx.subscribe_id.as_deref(),
            )
    })
}

struct WasmPolicyInvoker {
    plugins: Arc<PluginService>,
    deps: HostDeps,
    plugin: PluginRecord,
    hook_id: String,
    allowed_permissions: HashSet<String>,
    authority_mode: PolicyAuthority,
    compiled: CompiledPolicy,
    trust: plugin_api::PluginTrust,
    default_timeout_ms: u64,
}

impl WasmPolicyInvoker {
    fn resolve_invocation_authority(
        &self,
        ctx: &HookContext,
    ) -> Result<Option<(PluginStateAuthority, Option<String>)>, Failure> {
        match &self.authority_mode {
            PolicyAuthority::Fixed {
                authority,
                credential_epoch,
            } => match self.deps.store.validate_plugin_state_authority(authority) {
                Ok(()) => Ok(Some((authority.clone(), credential_epoch.clone()))),
                Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
                    Ok(None)
                }
                Err(err) => Err(Failure::Guest(map_store_error(err))),
            },
            PolicyAuthority::PerInvocationUnbound => {
                match self
                    .deps
                    .store
                    .issue_plugin_state_authority(&self.plugin, None)
                {
                    Ok(authority) => Ok(Some((authority, None))),
                    Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
                        Ok(None)
                    }
                    Err(err) => Err(Failure::Guest(map_store_error(err))),
                }
            }
            PolicyAuthority::PerInvocationSite => {
                let Some(raw_site) = ctx
                    .site_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return Ok(None);
                };
                let Ok(sid) = SiteId::from_str(strip_site_prefix(raw_site)) else {
                    return Ok(None);
                };
                let Some(obs) = self
                    .deps
                    .store
                    .observe_site(sid)
                    .map_err(map_store_error)
                    .map_err(Failure::Guest)?
                else {
                    return Ok(None);
                };
                if !obs.site().enabled {
                    return Ok(None);
                }
                let epoch = obs.credential_epoch().storage_key().to_string();
                match self
                    .deps
                    .store
                    .issue_plugin_state_authority(&self.plugin, Some(&obs))
                {
                    Ok(authority) => Ok(Some((authority, Some(epoch)))),
                    Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
                        Ok(None)
                    }
                    Err(err) => Err(Failure::Guest(map_store_error(err))),
                }
            }
        }
    }

    fn prepare_invocation(
        &self,
        role: GuestRole,
        policy_role: PluginInvocationRole,
        ctx: &HookContext,
    ) -> Result<Option<(Invocation, InvocationGuard)>, Failure> {
        let mut grants = self
            .deps
            .store
            .get_plugin_grants(&self.plugin.plugin_id)
            .map_err(|e| Failure::Trap(e.to_string()))?;
        grants.retain(|g| self.allowed_permissions.contains(&g.capability));
        if !hook_grants_cover_context(&grants, &self.hook_id, ctx) {
            return Ok(None);
        }
        if role == GuestRole::Notify
            && !cap_grants_cover_context(&grants, &format!("events:{}", self.hook_id), ctx)
        {
            return Ok(None);
        }
        let Some((authority, cred_epoch)) = self.resolve_invocation_authority(ctx)? else {
            return Ok(None);
        };
        let (manifest_http, max_response_bytes) = StoreHostPorts::load_manifest_http(
            &self.deps.store,
            &self.plugin.plugin_id,
            &self.plugin.package_hash,
        );
        let budget = Duration::from_millis(self.default_timeout_ms.max(100));
        let mut guest_inv = Invocation::new(self.plugin.plugin_id.clone(), budget);
        guest_inv.generation = u64::try_from(authority.generation()).unwrap_or(0);
        guest_inv.config_revision = u64::try_from(self.plugin.config_revision).unwrap_or(0);
        guest_inv.subject = ctx.subject.clone();
        guest_inv.site_id = authority
            .site_id()
            .map(|id| id.to_string())
            .or_else(|| ctx.site_id.clone());
        guest_inv.credential_epoch = cred_epoch;
        guest_inv.subscribe_id = ctx.subscribe_id.clone();
        guest_inv.library_id = ctx.library_id.clone();
        if !ctx.cause_id.is_empty() {
            guest_inv.cause_id = ctx.cause_id.clone();
        }
        guest_inv.trust = self.trust;
        guest_inv.role = role;

        let flight = self.plugins.in_flight().try_enter_owned(
            &self.plugin.plugin_id,
            authority.generation(),
            &guest_inv,
        )?;
        let invocation_site = StoreHostPorts::try_resolve_optional_site(&self.deps, &guest_inv)
            .map_err(Failure::Guest)?;

        let subject = PluginSubject::Task {
            plugin_id: self.plugin.plugin_id.clone(),
            task_id: format!("hook:{}", ctx.invocation_id),
        };
        let cause = PluginCommandCause::root(guest_inv.cause_id.clone());
        let guard = self.plugins.ports().register_with_flight(
            guest_inv.id().to_string(),
            ActiveInvocation {
                deps: self.deps.clone(),
                auth: ActiveAuth::Runtime {
                    authority,
                    grants,
                    invocation_site,
                },
                subject,
                cause,
                role: policy_role,
                manifest_http,
                max_response_bytes,
            },
            Some(flight),
        );
        Ok(Some((guest_inv, guard)))
    }
}

impl PluginInvoker for WasmPolicyInvoker {
    fn filter_typed(&self, input: &FilterInput) -> Result<Vec<Patch>, Failure> {
        let host = self.plugins.wasm_host()?;
        let Some((guest_inv, _guard)) = self.prepare_invocation(
            GuestRole::Filter,
            PluginInvocationRole::Filter,
            &input.context,
        )?
        else {
            return Ok(Vec::new());
        };
        let res = host.invoke_filter(&self.compiled, guest_inv, input.clone());
        if res.is_err() {
            self.plugins
                .in_flight()
                .record_error(&self.plugin.plugin_id);
        }
        res
    }

    fn gate_typed(&self, input: &GateInput) -> Result<Gate, String> {
        let host = self.plugins.wasm_host().map_err(|e| e.to_string())?;
        let Some((guest_inv, _guard)) = self
            .prepare_invocation(GuestRole::Gate, PluginInvocationRole::Gate, &input.context)
            .map_err(|e| e.to_string())?
        else {
            return Ok(Gate::Abstain);
        };
        let res = host.invoke_gate(&self.compiled, guest_inv, input.clone());
        if res.is_err() {
            self.plugins
                .in_flight()
                .record_error(&self.plugin.plugin_id);
        }
        res.map_err(|e| e.to_string())
    }

    fn notify_typed(&self, event: &Event) -> Result<(), String> {
        self.notify_typed_result(event).map_err(|e| e.to_string())
    }

    fn notify_typed_result(&self, event: &Event) -> Result<(), Failure> {
        let host = self.plugins.wasm_host()?;
        let Some((guest_inv, _guard)) = self.prepare_invocation(
            GuestRole::Notify,
            PluginInvocationRole::Notify,
            &event.context,
        )?
        else {
            return Err(Failure::Guest(api_err(
                plugin_api::ErrorCode::PermissionDenied,
                "event delivery grant or authority unavailable",
            )));
        };
        let res = host.invoke_notify(&self.compiled, guest_inv, event.clone());
        if res.is_err() {
            self.plugins
                .in_flight()
                .record_error(&self.plugin.plugin_id);
        }
        res
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_wasm_hook_invoker(
    plugins: &Arc<PluginService>,
    deps: &HostDeps,
    host: &plugin_host::PluginHost,
    plugin: &PluginRecord,
    comp: &plugin_package::ComponentManifest,
    hook_id: &str,
    authority_mode: PolicyAuthority,
    trust: plugin_api::PluginTrust,
    timeout_ms: u64,
) -> Option<Arc<dyn hooks::PluginInvoker>> {
    let Ok(compiled) =
        plugins
            .cache()
            .get_or_load_policy_from_store(&deps.store, host, plugin, &comp.entry)
    else {
        tracing::warn!(
            plugin_id = %plugin.plugin_id,
            hook_id = %hook_id,
            "failed to load policy component for hook"
        );
        return None;
    };
    Some(Arc::new(WasmPolicyInvoker {
        plugins: plugins.clone(),
        deps: deps.clone(),
        plugin: plugin.clone(),
        hook_id: hook_id.to_string(),
        allowed_permissions: comp.permissions.iter().cloned().collect(),
        authority_mode,
        compiled,
        trust,
        default_timeout_ms: timeout_ms,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_hook_invoker(
    state: &ApiState,
    host: &plugin_host::PluginHost,
    plugin: &PluginRecord,
    version: &PluginVersionMetadata,
    comp: &plugin_package::ComponentManifest,
    hook_id: &str,
    authority_mode: PolicyAuthority,
    trust: plugin_api::PluginTrust,
    timeout_ms: u64,
) -> Option<Arc<dyn hooks::PluginInvoker>> {
    if version.signature_status == "builtin" && comp.entry == "builtin" {
        return match (plugin.plugin_id.as_str(), hook_id) {
            (crate::plugins::boost::BUILTIN_BOOST_PLUGIN_ID, "download.before") => Some(Arc::new(
                crate::plugins::boost::BuiltinBoostDownloadGate::new(state.clone(), plugin.clone()),
            )),
            _ => None,
        };
    }
    let deps = HostDeps::from_state(state);
    resolve_wasm_hook_invoker(
        &state.plugins,
        &deps,
        host,
        plugin,
        comp,
        hook_id,
        authority_mode,
        trust,
        timeout_ms,
    )
}
