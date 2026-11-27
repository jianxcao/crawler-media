mod authz;
mod boost_download;
mod cookies_and_manifest;
mod http;
mod ops_and_errors;
mod site;
#[cfg(test)]
mod tests;

pub(crate) use cookies_and_manifest::CapturedCookieRecord;
use cookies_and_manifest::{CapturedCookieBook, filter_grants_for_manifest_invocation};
pub(crate) use ops_and_errors::{api_err, map_store_error};
use ops_and_errors::{parse_etag, state_entry_dto};

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use domain::SiteId;
use net_guard::{NetGuardClient, TargetPolicy};
use parking_lot::{Mutex, RwLock};
use plugin_api::{
    ApiError, BoostSnapshotView, BrowserPurpose, CommandRequest, ConfigView, EntryPage, ErrorCode,
    Failure, GuestRole, HttpRequest, HttpResponse, LogLevel, OperationPage, OperationView,
    PageRequest, SitePage, SiteView, StateEntry, StatsView,
};
use plugin_host::{BrowserSessionPort, HostPorts, Invocation, KvValue};
use plugin_package::{HttpDestinationManifest, PluginManifest};
use store::Store;
use store::plugins::{
    PluginCommandCause, PluginConfig, PluginGrant, PluginLifecycle, PluginStateAuthority,
    PluginStateEntry, PluginStateKind, PluginSubject,
};

use super::effect::{PluginEffectAdapter, PluginInvocationRole};
use super::wasm_runner::{InFlightRegistry, OwnedInFlightGuard};
use crate::JobsWriter;
use crate::management::ApiState;

pub(crate) use authz::ensure_provider_role_grant;
pub(crate) use site::{
    parse_downloader_scope_id, parse_library_scope_id, parse_site_scope_id,
    parse_subscribe_scope_id, strip_site_prefix,
};

#[derive(Clone)]
pub(crate) struct HostDeps {
    pub store: Arc<Store>,
    pub jobs: Arc<JobsWriter>,
    pub effects: Arc<dyn PluginEffectAdapter>,
    pub in_flight: Arc<InFlightRegistry>,
    pub browser: Arc<indexer::Browser>,
    pub downloader: Arc<dyn downloader::Downloader>,
    pub instance_writable: Arc<std::sync::atomic::AtomicBool>,
    pub write_gate: Arc<crate::write_gate::WriteGate>,
    pub self_listen: Option<SocketAddr>,
    pub plugin_http_policy_override: Option<TargetPolicy>,
    pub admin_trusted_plugins: HashSet<String>,
}

impl HostDeps {
    pub(crate) fn from_state(state: &ApiState) -> Self {
        Self {
            store: state.store.clone(),
            jobs: state.jobs.clone(),
            effects: state.plugins.effects().clone(),
            in_flight: state.plugins.in_flight().clone(),
            browser: state.browser.clone(),
            downloader: state.downloader.clone(),
            instance_writable: state.instance_writable.clone(),
            write_gate: state.write_gate.clone(),
            self_listen: state.self_listen,
            plugin_http_policy_override: state.plugin_http_policy_override.clone(),
            admin_trusted_plugins: state.admin_trusted_plugins.clone(),
        }
    }

    pub(super) fn ensure_write_admitted(&self) -> Result<(), ApiError> {
        if !self
            .instance_writable
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(api_err(ErrorCode::Conflict, "instance is read-only"));
        }
        if self.write_gate.is_closed() {
            return Err(api_err(ErrorCode::Busy, "maintenance write gate closed"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) enum ActiveAuth {
    Lifecycle {
        plugin_id: String,
        config: Option<PluginConfig>,
    },
    Runtime {
        authority: PluginStateAuthority,
        grants: Vec<PluginGrant>,
        invocation_site: Option<(SiteId, Option<String>)>,
    },
}

#[derive(Clone)]
pub(crate) struct ActiveInvocation {
    pub deps: HostDeps,
    pub auth: ActiveAuth,
    pub subject: PluginSubject,
    pub cause: PluginCommandCause,
    pub role: PluginInvocationRole,
    pub manifest_http: Arc<Vec<HttpDestinationManifest>>,
    pub max_response_bytes: Option<u64>,
}

pub(crate) struct InvocationGuard {
    active: Arc<Mutex<HashMap<String, ActiveInvocation>>>,
    id: String,
    _flight: Option<OwnedInFlightGuard>,
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.active.lock().remove(&self.id);
    }
}

pub(crate) struct StoreHostPorts {
    active: Arc<Mutex<HashMap<String, ActiveInvocation>>>,
    deps: RwLock<Option<HostDeps>>,
    http_client: NetGuardClient,
    captured_cookies: Arc<Mutex<CapturedCookieBook>>,
}

impl Default for StoreHostPorts {
    fn default() -> Self {
        Self {
            active: Arc::new(Mutex::new(HashMap::new())),
            deps: RwLock::new(None),
            http_client: NetGuardClient::new(),
            captured_cookies: Arc::new(Mutex::new(CapturedCookieBook::default())),
        }
    }
}

impl StoreHostPorts {
    pub(crate) fn clone_handle(&self) -> Self {
        Self {
            active: self.active.clone(),
            deps: RwLock::new(self.deps.read().clone()),
            http_client: self.http_client.clone(),
            captured_cookies: self.captured_cookies.clone(),
        }
    }

    pub(crate) fn bind_deps(&self, deps: HostDeps) {
        *self.deps.write() = Some(deps);
    }

    pub(crate) fn register(&self, id: String, ctx: ActiveInvocation) -> InvocationGuard {
        self.register_with_flight(id, ctx, None)
    }

    pub(crate) fn register_with_flight(
        &self,
        id: String,
        ctx: ActiveInvocation,
        flight: Option<OwnedInFlightGuard>,
    ) -> InvocationGuard {
        self.active.lock().insert(id.clone(), ctx);
        InvocationGuard {
            active: self.active.clone(),
            id,
            _flight: flight,
        }
    }

    pub(crate) fn load_manifest_http(
        store: &Store,
        plugin_id: &str,
        package_hash: &str,
    ) -> (Arc<Vec<HttpDestinationManifest>>, Option<u64>) {
        let Ok(Some(ver)) = store.get_plugin_version(plugin_id, package_hash) else {
            return (Arc::new(Vec::new()), None);
        };
        if ver.manifest_json == "{}" {
            return (Arc::new(Vec::new()), None);
        }
        let Ok(manifest) = serde_json::from_str::<PluginManifest>(&ver.manifest_json) else {
            return (Arc::new(Vec::new()), None);
        };
        let max_bytes = manifest.limits.and_then(|l| l.response_bytes);
        (Arc::new(manifest.http), max_bytes)
    }

    pub(crate) fn record_captured_cookies(&self, record: CapturedCookieRecord) -> String {
        self.captured_cookies.lock().record(record)
    }

    pub(crate) fn take_captured_cookies(&self, token: &str) -> Option<CapturedCookieRecord> {
        self.captured_cookies.lock().take(token)
    }

    pub(crate) fn site_http_policy_parts(&self) -> (Option<TargetPolicy>, Option<SocketAddr>) {
        match self.deps.read().as_ref() {
            Some(deps) => (deps.plugin_http_policy_override.clone(), deps.self_listen),
            None => (None, None),
        }
    }

    pub(super) fn lookup(&self, inv: &Invocation) -> Result<ActiveInvocation, ApiError> {
        self.active
            .lock()
            .get(inv.id())
            .cloned()
            .ok_or_else(|| api_err(ErrorCode::PermissionDenied, "inactive plugin invocation"))
    }

    pub(super) fn runtime_authority<'a>(
        &self,
        ctx: &'a ActiveInvocation,
    ) -> Result<&'a PluginStateAuthority, ApiError> {
        match &ctx.auth {
            ActiveAuth::Runtime { authority, .. } => Ok(authority),
            ActiveAuth::Lifecycle { .. } => Err(api_err(
                ErrorCode::WrongContext,
                "port unavailable during lifecycle validation",
            )),
        }
    }

    fn state_get(
        &self,
        inv: &Invocation,
        kind: PluginStateKind,
        key: &str,
    ) -> Result<Option<PluginStateEntry>, ApiError> {
        let ctx = self.lookup(inv)?;
        let authority = self.runtime_authority(&ctx)?;
        if !self.authorized(inv, kind.capability()) {
            return Err(api_err(ErrorCode::PermissionDenied, "permission denied"));
        }
        ctx.deps
            .store
            .plugin_state_get(authority, kind, key)
            .map_err(map_store_error)
    }

    fn state_list(
        &self,
        inv: &Invocation,
        kind: PluginStateKind,
        prefix: &str,
        page: PageRequest,
    ) -> Result<EntryPage, ApiError> {
        let ctx = self.lookup(inv)?;
        let authority = self.runtime_authority(&ctx)?;
        if !self.authorized(inv, kind.capability()) {
            return Err(api_err(ErrorCode::PermissionDenied, "permission denied"));
        }
        let limit = (page.limit.max(1) as usize).min(100);
        let entries = ctx
            .deps
            .store
            .plugin_state_list_entries(authority, kind, prefix, page.cursor.as_deref(), Some(limit))
            .map_err(map_store_error)?;
        let cap = if kind == PluginStateKind::Blob {
            limit.min(32)
        } else {
            limit
        };
        let next_cursor = if entries.len() == cap {
            entries.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        Ok(EntryPage {
            items: entries.into_iter().map(state_entry_dto).collect(),
            next_cursor,
        })
    }

    fn check_state_write_role(
        inv_role: GuestRole,
        ctx_role: PluginInvocationRole,
        kind: PluginStateKind,
    ) -> Result<(), ApiError> {
        let role_forbidden = matches!(
            inv_role,
            GuestRole::Lifecycle | GuestRole::Filter | GuestRole::Gate | GuestRole::Notify
        ) || matches!(
            ctx_role,
            PluginInvocationRole::Lifecycle
                | PluginInvocationRole::Filter
                | PluginInvocationRole::Gate
                | PluginInvocationRole::Notify
                | PluginInvocationRole::ReadOnlyEndpoint
        ) || (kind == PluginStateKind::Blob
            && (!matches!(inv_role, GuestRole::Task | GuestRole::Endpoint)
                || !matches!(
                    ctx_role,
                    PluginInvocationRole::Task | PluginInvocationRole::Endpoint
                )));
        if role_forbidden {
            return Err(api_err(
                ErrorCode::WrongContext,
                "state writes are forbidden in this role",
            ));
        }
        Ok(())
    }

    fn state_put(
        &self,
        inv: &Invocation,
        kind: PluginStateKind,
        key: &str,
        value: &[u8],
        if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        if matches!(
            inv.role,
            GuestRole::Lifecycle | GuestRole::Filter | GuestRole::Gate | GuestRole::Notify
        ) || (kind == PluginStateKind::Blob
            && !matches!(inv.role, GuestRole::Task | GuestRole::Endpoint))
        {
            return Err(api_err(
                ErrorCode::WrongContext,
                "state writes are forbidden in this role",
            ));
        }
        let ctx = self.lookup(inv)?;
        Self::check_state_write_role(inv.role, ctx.role, kind)?;
        ctx.deps.ensure_write_admitted()?;
        let authority = self.runtime_authority(&ctx)?;
        if !self.authorized(inv, kind.capability()) {
            return Err(api_err(ErrorCode::PermissionDenied, "permission denied"));
        }
        let expected = match if_match {
            Some(tag) => parse_etag(tag, true)?,
            None => ctx
                .deps
                .store
                .plugin_state_get(authority, kind, key)
                .map_err(map_store_error)?
                .map_or(0, |entry| entry.revision),
        };
        let entry = ctx
            .deps
            .store
            .plugin_state_put(authority, kind, key, value, expected)
            .map_err(map_store_error)?;
        Ok(entry.revision.to_string())
    }

    fn state_delete(
        &self,
        inv: &Invocation,
        kind: PluginStateKind,
        key: &str,
        if_match: Option<&str>,
    ) -> Result<(), ApiError> {
        if matches!(
            inv.role,
            GuestRole::Lifecycle | GuestRole::Filter | GuestRole::Gate | GuestRole::Notify
        ) || (kind == PluginStateKind::Blob
            && !matches!(inv.role, GuestRole::Task | GuestRole::Endpoint))
        {
            return Err(api_err(
                ErrorCode::WrongContext,
                "state writes are forbidden in this role",
            ));
        }
        let ctx = self.lookup(inv)?;
        Self::check_state_write_role(inv.role, ctx.role, kind)?;
        ctx.deps.ensure_write_admitted()?;
        let authority = self.runtime_authority(&ctx)?;
        if !self.authorized(inv, kind.capability()) {
            return Err(api_err(ErrorCode::PermissionDenied, "permission denied"));
        }
        let expected = match if_match {
            Some(tag) => parse_etag(tag, false)?,
            None => {
                ctx.deps
                    .store
                    .plugin_state_get(authority, kind, key)
                    .map_err(map_store_error)?
                    .ok_or_else(|| api_err(ErrorCode::NotFound, "missing state entry"))?
                    .revision
            }
        };
        ctx.deps
            .store
            .plugin_state_delete(authority, kind, key, expected)
            .map_err(map_store_error)
    }
}

fn map_failure_to_api(failure: Failure) -> ApiError {
    match failure {
        Failure::Guest(err) => err,
        Failure::Busy => api_err(ErrorCode::Busy, "plugin dispatch busy"),
        Failure::InvalidInput(msg) => api_err(ErrorCode::InvalidArgument, &msg),
        other => api_err(ErrorCode::Conflict, &other.to_string()),
    }
}

impl StoreHostPorts {
    fn begin_lifecycle_invocation(
        &self,
        deps: HostDeps,
        inv: &Invocation,
    ) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>, ApiError> {
        let config = deps
            .store
            .get_plugin_config(&inv.plugin_id)
            .map_err(map_store_error)?;
        let guard = self.register(
            inv.id().to_string(),
            ActiveInvocation {
                deps,
                auth: ActiveAuth::Lifecycle {
                    plugin_id: inv.plugin_id.clone(),
                    config,
                },
                subject: PluginSubject::Task {
                    plugin_id: inv.plugin_id.clone(),
                    task_id: "lifecycle".into(),
                },
                cause: PluginCommandCause::root(inv.cause_id.clone()),
                role: PluginInvocationRole::Lifecycle,
                manifest_http: Arc::new(Vec::new()),
                max_response_bytes: None,
            },
        );
        Ok(Some(Box::new(guard)))
    }
}

impl HostPorts for StoreHostPorts {
    fn begin_invocation(
        &self,
        inv: &Invocation,
    ) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>, ApiError> {
        if self.active.lock().contains_key(inv.id()) {
            return Ok(None);
        }
        if inv.cancelled() {
            return Err(api_err(ErrorCode::Cancelled, "plugin invocation cancelled"));
        }
        if inv.expired() {
            return Err(api_err(
                ErrorCode::DeadlineExceeded,
                "plugin invocation deadline expired",
            ));
        }
        let Some(deps) = self.deps.read().clone() else {
            return Err(api_err(
                ErrorCode::Unsupported,
                "host ports not initialized",
            ));
        };
        if inv.role == GuestRole::Lifecycle {
            return self.begin_lifecycle_invocation(deps, inv);
        }

        let plugin = deps
            .store
            .get_plugin(&inv.plugin_id)
            .map_err(map_store_error)?
            .ok_or_else(|| api_err(ErrorCode::NotFound, "plugin not found"))?;
        if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
            return Err(api_err(
                ErrorCode::StaleGeneration,
                "plugin is not enabled for runtime dispatch",
            ));
        }
        if inv.generation != u64::try_from(plugin.generation).unwrap_or(0) {
            return Err(api_err(
                ErrorCode::StaleGeneration,
                "stale plugin generation",
            ));
        }
        let version = deps
            .store
            .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
            .map_err(map_store_error)?
            .ok_or_else(|| api_err(ErrorCode::NotFound, "plugin version metadata missing"))?;
        let mut grants = deps
            .store
            .get_plugin_grants(&plugin.plugin_id)
            .map_err(map_store_error)?;
        filter_grants_for_manifest_invocation(&plugin.plugin_id, &version, inv, &mut grants);
        let (site_id_parsed, obs) = Self::resolve_invocation_site(&deps, inv)?;
        authz::ensure_provider_role_grant(inv.role, &grants, site_id_parsed)?;
        let site_ref = if version.credential_bound {
            Some(obs.as_ref().ok_or_else(|| {
                api_err(
                    ErrorCode::NotFound,
                    "credential-bound invocation missing site observation",
                )
            })?)
        } else {
            None
        };
        let authority = deps
            .store
            .issue_plugin_state_authority(&plugin, site_ref)
            .map_err(map_store_error)?;
        let (manifest_http, max_response_bytes) =
            Self::load_manifest_http(&deps.store, &plugin.plugin_id, &plugin.package_hash);
        let flight = deps
            .in_flight
            .try_enter_owned(&plugin.plugin_id, plugin.generation, inv)
            .map_err(map_failure_to_api)?;

        let active_ctx = ActiveInvocation {
            deps,
            auth: ActiveAuth::Runtime {
                authority,
                grants,
                invocation_site: site_id_parsed.map(|sid| (sid, inv.credential_epoch.clone())),
            },
            subject: PluginSubject::Task {
                plugin_id: plugin.plugin_id,
                task_id: format!("{:?}", inv.role),
            },
            cause: PluginCommandCause::root(inv.cause_id.clone()),
            role: PluginInvocationRole::from_guest_role(inv.role),
            manifest_http,
            max_response_bytes,
        };
        let id = inv.id().to_string();
        self.active.lock().insert(id.clone(), active_ctx);
        Ok(Some(Box::new(InvocationGuard {
            active: self.active.clone(),
            id,
            _flight: Some(flight),
        })))
    }

    fn authorized(&self, inv: &Invocation, cap: &str) -> bool {
        let guard = self.active.lock();
        let Some(ctx) = guard.get(inv.id()) else {
            return false;
        };
        authz::is_capability_authorized(&ctx.auth, cap)
    }

    fn config_get(&self, inv: &Invocation) -> Result<ConfigView, ApiError> {
        let ctx = self.lookup(inv)?;
        let cfg = match &ctx.auth {
            ActiveAuth::Runtime { authority, .. } => {
                ctx.deps
                    .store
                    .validate_plugin_state_authority(authority)
                    .map_err(map_store_error)?;
                ctx.deps
                    .store
                    .get_plugin_config(authority.plugin_id())
                    .map_err(map_store_error)?
            }
            ActiveAuth::Lifecycle { config, .. } => config.clone(),
        };
        match cfg {
            Some(cfg) => {
                let validated_json = serde_json::to_string(&cfg.values)
                    .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
                Ok(ConfigView {
                    revision: u64::try_from(cfg.revision).unwrap_or(0),
                    validated_json,
                })
            }
            None => Ok(ConfigView {
                revision: 0,
                validated_json: "{}".into(),
            }),
        }
    }

    fn log_write(
        &self,
        inv: &Invocation,
        level: LogLevel,
        message: &str,
        fields: &[(String, String)],
    ) -> Result<(), ApiError> {
        let ctx = self.lookup(inv)?;
        let plugin_id = match &ctx.auth {
            ActiveAuth::Runtime { authority, .. } => {
                ctx.deps
                    .store
                    .validate_plugin_state_authority(authority)
                    .map_err(map_store_error)?;
                authority.plugin_id().to_string()
            }
            ActiveAuth::Lifecycle { plugin_id, .. } => plugin_id.clone(),
        };
        match level {
            LogLevel::Debug => tracing::debug!(%plugin_id, ?fields, "{message}"),
            LogLevel::Info => tracing::info!(%plugin_id, ?fields, "{message}"),
            LogLevel::Warn => tracing::warn!(%plugin_id, ?fields, "{message}"),
            LogLevel::Error => tracing::error!(%plugin_id, ?fields, "{message}"),
        }
        Ok(())
    }

    fn kv_get(&self, inv: &Invocation, key: &str) -> Result<Option<KvValue>, ApiError> {
        Ok(self
            .state_get(inv, PluginStateKind::Kv, key)?
            .map(|entry| KvValue {
                value: entry.value.as_bytes().to_vec(),
                etag: entry.revision.to_string(),
            }))
    }

    fn kv_list(
        &self,
        inv: &Invocation,
        prefix: &str,
        page: PageRequest,
    ) -> Result<EntryPage, ApiError> {
        self.state_list(inv, PluginStateKind::Kv, prefix, page)
    }

    fn kv_put(
        &self,
        inv: &Invocation,
        key: &str,
        value: &[u8],
        if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        self.state_put(inv, PluginStateKind::Kv, key, value, if_match)
    }

    fn kv_delete(
        &self,
        inv: &Invocation,
        key: &str,
        if_match: Option<&str>,
    ) -> Result<(), ApiError> {
        self.state_delete(inv, PluginStateKind::Kv, key, if_match)
    }

    fn blob_get(&self, inv: &Invocation, key: &str) -> Result<StateEntry, ApiError> {
        self.state_get(inv, PluginStateKind::Blob, key)?
            .map(state_entry_dto)
            .ok_or_else(|| api_err(ErrorCode::NotFound, "missing blob entry"))
    }

    fn blob_list(
        &self,
        inv: &Invocation,
        prefix: &str,
        page: PageRequest,
    ) -> Result<EntryPage, ApiError> {
        self.state_list(inv, PluginStateKind::Blob, prefix, page)
    }

    fn blob_put(
        &self,
        inv: &Invocation,
        key: &str,
        value: &[u8],
        if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        self.state_put(inv, PluginStateKind::Blob, key, value, if_match)
    }

    fn blob_delete(
        &self,
        inv: &Invocation,
        key: &str,
        if_match: Option<&str>,
    ) -> Result<(), ApiError> {
        self.state_delete(inv, PluginStateKind::Blob, key, if_match)
    }

    fn operations_get(&self, inv: &Invocation, id: &str) -> Result<OperationView, ApiError> {
        ops_and_errors::operations_get(self, inv, id)
    }

    fn operations_list(
        &self,
        inv: &Invocation,
        page: PageRequest,
    ) -> Result<OperationPage, ApiError> {
        ops_and_errors::operations_list(self, inv, page)
    }

    fn commands_submit(&self, inv: &Invocation, req: CommandRequest) -> Result<String, ApiError> {
        ops_and_errors::commands_submit(self, inv, req)
    }

    fn http_fetch(&self, inv: &Invocation, req: HttpRequest) -> Result<HttpResponse, ApiError> {
        http::http_fetch(self, &http::InvocationProxy { inv }, req)
    }

    fn http_fetch_many(
        &self,
        inv: &Invocation,
        reqs: Vec<HttpRequest>,
    ) -> Vec<Result<HttpResponse, ApiError>> {
        http::http_fetch_many(self, inv, reqs)
    }

    fn site_list(&self, inv: &Invocation, page: PageRequest) -> Result<SitePage, ApiError> {
        site::site_list(self, inv, page)
    }

    fn site_get(&self, inv: &Invocation, handle: &str) -> Result<SiteView, ApiError> {
        site::site_get(self, inv, handle)
    }

    fn site_stats(&self, inv: &Invocation, handle: &str) -> Result<StatsView, ApiError> {
        site::site_stats(self, inv, handle)
    }

    fn site_credential_epoch(&self, handle: &str) -> Result<Option<String>, ApiError> {
        site::site_credential_epoch(self, handle)
    }

    fn browser_open(
        &self,
        inv: &Invocation,
        purpose: BrowserPurpose,
    ) -> Result<Box<dyn BrowserSessionPort>, ApiError> {
        super::browser_adapter::open_browser_session(self, inv, purpose)
    }

    fn boost_snapshot(
        &self,
        inv: &Invocation,
        sites: &[String],
        downloaders: &[String],
    ) -> Result<BoostSnapshotView, ApiError> {
        boost_download::boost_snapshot(self, inv, sites, downloaders)
    }
}
