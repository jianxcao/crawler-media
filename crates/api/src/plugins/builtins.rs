use std::sync::{Arc, Weak};

use domain::{MaintenanceVia, Site, SiteId};
use hooks::{
    CheckInPlugin, HookPipeline, LoginPlugin, NexusPhpPolicy, PluginError, SiteCredentials,
    SiteResponsePolicy,
};
use indexer::{
    Browser, FetchMethod, FetchRequest, Indexer, IndexerError, MaintenanceAction,
    MaintenanceCallContext, MaintenanceDescriptor, MaintenanceOutcome, MaintenanceProvider,
    MaintenanceState, ProviderAvailability, ProviderSource,
};
use parking_lot::Mutex;
use plugin_package::{
    ComponentManifest, MaintenanceManifest, PluginManifest, TaskManifest, TaskScheduleManifest,
};
use serde_json::json;
use store::Store;
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycleObservation, PluginTaskDefinition,
    PluginVersionMetadata,
};
use uuid::Uuid;

use super::effect::PluginTaskInvocation;
use super::ports::{CapturedCookieRecord, StoreHostPorts};
use super::tasks::sync_task_binding;
use crate::management::ApiState;

pub const BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID: &str = "builtin.nexusphp-maintenance";
pub const BUILTIN_NEXUSPHP_MAINTENANCE_PROVIDER_ID: &str =
    "plugin:builtin.nexusphp-maintenance/default";
pub const BUILTIN_CHECKIN_PLUGIN_ID: &str = "builtin.checkin";
pub const BUILTIN_CHECKIN_TASK_ID: &str = "check-in";

fn nexusphp_manifest_json() -> String {
    let manifest = PluginManifest {
        manifest_version: 1,
        id: BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID.into(),
        name: "NexusPHP Site Maintenance".into(),
        version: "1.0.0".into(),
        api: plugin_api::API_VERSION.into(),
        license: "Apache-2.0".into(),
        authors: vec!["crawler-media".into()],
        description: "Built-in NexusPHP session verification and check-in provider".into(),
        config_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#.into(),
        state_schema: 1,
        credential_binding: Some("invocation-site".into()),
        http: Vec::new(),
        sites: Vec::new(),
        limits: None,
        components: vec![ComponentManifest {
            id: "builtin".into(),
            entry: "builtin".into(),
            world: plugin_host::MAINTENANCE_WORLD.into(),
            permissions: vec![
                "http".into(),
                "http:site-auth".into(),
                "read:sites".into(),
                "maintenance:provide".into(),
            ],
            hooks: Vec::new(),
            tasks: Vec::new(),
            indexers: Vec::new(),
            maintenance: vec![MaintenanceManifest {
                id: "default".into(),
                actions: vec!["verify-session".into(), "check-in".into()],
            }],
            endpoints: Vec::new(),
            lists: Vec::new(),
            channels: Vec::new(),
        }],
        ui: None,
    };
    serde_json::to_string(&manifest).unwrap_or_else(|_| "{}".into())
}

const CHECKIN_CONFIG_SCHEMA_JSON: &str = r#"{"type":"object","properties":{"interval_secs":{"type":"integer","minimum":3600,"maximum":86400,"default":86400},"site_id":{"type":"string"}},"additionalProperties":false}"#;

fn checkin_manifest_json() -> String {
    let input_schema = r#"{"type":"object","properties":{"interval_secs":{"type":"integer","minimum":3600,"maximum":86400,"default":86400},"site_id":{"type":"string"}},"additionalProperties":false}"#;
    let manifest = PluginManifest {
        manifest_version: 1,
        id: BUILTIN_CHECKIN_PLUGIN_ID.into(),
        name: "Site Check-in Orchestrator".into(),
        version: "1.0.0".into(),
        api: plugin_api::API_VERSION.into(),
        license: "Apache-2.0".into(),
        authors: vec!["crawler-media".into()],
        description: "Built-in periodic site check-in and maintenance orchestrator".into(),
        config_schema: CHECKIN_CONFIG_SCHEMA_JSON.into(),
        state_schema: 1,
        credential_binding: None,
        http: Vec::new(),
        sites: Vec::new(),
        limits: None,
        components: vec![ComponentManifest {
            id: "builtin".into(),
            entry: "builtin".into(),
            world: plugin_host::TASK_WORLD.into(),
            permissions: vec![
                "command:site.maintenance".into(),
                "command:site.check-in".into(),
                "command:site.verify-session".into(),
                "browser:maintenance".into(),
                "kv".into(),
                "read:sites".into(),
            ],
            hooks: Vec::new(),
            tasks: vec![TaskManifest {
                id: BUILTIN_CHECKIN_TASK_ID.into(),
                input_schema: input_schema.into(),
                schedule: TaskScheduleManifest {
                    kind: "interval".into(),
                    seconds: 86_400,
                },
                concurrency_key: None,
            }],
            indexers: Vec::new(),
            maintenance: Vec::new(),
            endpoints: Vec::new(),
            lists: Vec::new(),
            channels: Vec::new(),
        }],
        ui: Some(super::ui_builtins::checkin_ui_manifest()),
    };
    serde_json::to_string(&manifest).unwrap_or_else(|_| "{}".into())
}

pub(super) fn ensure_builtin_plugin(
    store: &Store,
    version: PluginVersionMetadata,
    grants: Vec<PluginGrant>,
) -> Result<(), String> {
    if store
        .get_plugin(&version.plugin_id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let record = store
        .register_plugin_version(&version, 0)
        .map_err(|e| e.to_string())?;
    let record = if !grants.is_empty() {
        store
            .grant_plugin_permissions(
                &record.plugin_id,
                &record.package_hash,
                record.revision,
                &grants,
            )
            .map_err(|e| e.to_string())?
    } else {
        record
    };
    let op_id = Uuid::new_v4();
    let snap = store
        .begin_plugin_enable_checked(&record.plugin_id, record.revision, op_id)
        .map_err(|e| e.to_string())?;
    store
        .finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: snap.plugin.plugin_id.clone(),
                revision: snap.plugin.revision,
                generation: snap.plugin.generation,
                operation_id: op_id,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn bootstrap_builtin_plugins(state: &ApiState) {
    let maint_ver = PluginVersionMetadata {
        plugin_id: BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID.into(),
        package_hash: "builtin:nexusphp-maintenance:1.0.0".into(),
        version: "1.0.0".into(),
        manifest_json: nexusphp_manifest_json(),
        config_schema_json: r#"{"type":"object","properties":{},"additionalProperties":false}"#
            .into(),
        state_schema: "1".into(),
        artifact_path: String::new(),
        signing_key_id: None,
        signature_status: "builtin".into(),
        ui_metadata_ref: None,
        credential_bound: true,
    };
    let maint_grants = vec![
        PluginGrant {
            capability: "http".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "http:site-auth".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "read:sites".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "maintenance:provide".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
    ];
    if let Err(err) = ensure_builtin_plugin(&state.store, maint_ver, maint_grants) {
        tracing::error!(error = %err, "Failed to bootstrap builtin.nexusphp-maintenance plugin");
    }

    let checkin_ver = PluginVersionMetadata {
        plugin_id: BUILTIN_CHECKIN_PLUGIN_ID.into(),
        package_hash: "builtin:checkin:1.0.0".into(),
        version: "1.0.0".into(),
        manifest_json: checkin_manifest_json(),
        config_schema_json: CHECKIN_CONFIG_SCHEMA_JSON.into(),
        state_schema: "1".into(),
        artifact_path: String::new(),
        signing_key_id: None,
        signature_status: "builtin".into(),
        ui_metadata_ref: Some("ui/asset-manifest.json".into()),
        credential_bound: false,
    };
    let checkin_grants = vec![
        PluginGrant {
            capability: "command:site.maintenance".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "command:site.check-in".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "command:site.verify-session".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "kv".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
        PluginGrant {
            capability: "read:sites".into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        },
    ];
    if let Err(err) = ensure_builtin_plugin(&state.store, checkin_ver, checkin_grants) {
        tracing::error!(error = %err, "Failed to bootstrap builtin.checkin plugin");
        return;
    }
    super::ui_builtins::ensure_builtin_ui_trust(
        &state.store,
        BUILTIN_CHECKIN_PLUGIN_ID,
        "builtin:checkin:1.0.0",
    );

    if let Ok(Some(plugin)) = state.store.get_plugin(BUILTIN_CHECKIN_PLUGIN_ID)
        && state
            .store
            .get_plugin_task_binding(BUILTIN_CHECKIN_PLUGIN_ID, BUILTIN_CHECKIN_TASK_ID)
            .ok()
            .flatten()
            .is_none()
        && let Ok(binding) = state.store.save_plugin_task_binding(
            &plugin,
            BUILTIN_CHECKIN_TASK_ID,
            0,
            &PluginTaskDefinition {
                enabled: false,
                interval_secs: Some(86_400),
                public_config: json!({}),
            },
        )
        && let Err(err) = sync_task_binding(state, &binding)
    {
        tracing::warn!(error = %err, "Failed to project builtin.checkin task binding on startup");
    }
    super::boost::bootstrap_builtin_boost(state);
}

fn authorize_checkin_site(
    store: &Store,
    authority: &store::plugins::PluginStateAuthority,
    site_id: SiteId,
) -> Result<(), String> {
    let target = [store::plugins::PluginCommandTarget::Site(site_id)];
    store
        .authorize_plugin_command(authority, "site.check-in", &target)
        .or_else(|_| store.authorize_plugin_command(authority, "site.maintenance", &target))
        .map(|_| ())
        .map_err(|e| format!("builtin.checkin unauthorized for site `{site_id}`: {e}"))
}

pub(crate) fn run_builtin_checkin_task(
    state: &ApiState,
    invocation: &PluginTaskInvocation,
) -> Result<(), String> {
    if invocation.binding.task_id != BUILTIN_CHECKIN_TASK_ID {
        return Err(format!(
            "unsupported builtin.checkin task `{}`",
            invocation.binding.task_id
        ));
    }
    let stored_cfg = state
        .store
        .get_plugin_config(BUILTIN_CHECKIN_PLUGIN_ID)
        .map_err(|e| e.to_string())?;
    let site_opt = invocation
        .input_override
        .as_ref()
        .and_then(|v| v.get("site_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            invocation
                .binding
                .definition
                .public_config
                .get("site_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            stored_cfg
                .as_ref()
                .and_then(|c| c.values.get("site_id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
    if let Some(site_str) = site_opt {
        let site_id: SiteId = super::ports::strip_site_prefix(site_str)
            .parse()
            .map_err(|_| format!("invalid site_id `{site_str}`"))?;
        authorize_checkin_site(&state.store, &invocation.authority, site_id)?;
        let obs = state
            .store
            .observe_site(site_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("site `{site_id}` not found"))?;
        if !obs.site().enabled {
            return Err(format!("site `{site_id}` is disabled"));
        }
        return crate::site_maintenance::run_scheduled_check_in_for_site(state, obs);
    }

    let grants = state
        .store
        .get_plugin_grants(BUILTIN_CHECKIN_PLUGIN_ID)
        .map_err(|e| e.to_string())?;
    if !grants.iter().any(|g| {
        matches!(
            g.capability.as_str(),
            "command:site.check-in" | "command:site.maintenance"
        )
    }) {
        return Err("builtin.checkin lacks check-in command grants".into());
    }
    let sites = state
        .store
        .list_enabled_sites()
        .map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    for site in sites {
        if authorize_checkin_site(&state.store, &invocation.authority, site.id).is_err() {
            continue;
        }
        let Some(observed) = state
            .store
            .observe_site(site.id)
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        let cur = observed.site().clone();
        if !cur.enabled {
            continue;
        }
        if let Err(error) =
            crate::site_maintenance::run_scheduled_check_in_for_site(state, observed)
        {
            tracing::error!(site = %cur.name, site_id = %cur.id, error = %error, "【站点签到】签到/保活失败");
            crate::worker::notify_public(state, &format!("站点签到失败：{}", cur.name), &error);
            failures.push(format!("{}: {error}", cur.name));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("站点签到失败: {}", failures.join("; ")))
    }
}

pub(crate) struct BuiltinNexusPhpMaintenanceProvider {
    descriptor: MaintenanceDescriptor,
    indexer: Weak<Indexer>,
    browser: Arc<Browser>,
    store: Arc<Store>,
    ports: Arc<StoreHostPorts>,
}

impl BuiltinNexusPhpMaintenanceProvider {
    pub(crate) fn new(
        plugin_generation: u64,
        indexer: Weak<Indexer>,
        browser: Arc<Browser>,
        store: Arc<Store>,
        ports: Arc<StoreHostPorts>,
    ) -> Self {
        Self {
            descriptor: MaintenanceDescriptor {
                id: BUILTIN_NEXUSPHP_MAINTENANCE_PROVIDER_ID.into(),
                source: ProviderSource::PluginWasm {
                    plugin_id: BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID.into(),
                    component_id: "builtin".into(),
                    version: "1.0.0".into(),
                    plugin_generation,
                },
                actions: vec![MaintenanceAction::VerifySession, MaintenanceAction::CheckIn],
                availability: ProviderAvailability::Enabled,
            },
            indexer,
            browser,
            store,
            ports,
        }
    }
}

struct CapturedSiteCredentials {
    site: Site,
    saved_cookie: Mutex<Option<String>>,
}

impl CapturedSiteCredentials {
    fn new(site: Site) -> Self {
        Self {
            site,
            saved_cookie: Mutex::new(None),
        }
    }

    fn take_cookie(&self) -> Option<String> {
        self.saved_cookie.lock().take()
    }
}

impl SiteCredentials for CapturedSiteCredentials {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        if id != self.site.id {
            return Err(PluginError::NotFound);
        }
        Ok(self.site.clone())
    }

    fn save(&self, site: &Site) -> Result<(), PluginError> {
        if site.id != self.site.id {
            return Err(PluginError::NotFound);
        }
        *self.saved_cookie.lock() = site.cookie.clone();
        Ok(())
    }
}

fn browser_endpoint_resolves(browser: &Browser, site: &Site) -> Result<bool, PluginError> {
    let config = browser
        .effective_config()
        .map_err(|e| PluginError::Fetch(format!("Browser 配置错误: {e}")))?;
    let resolved = config
        .resolve(site.cdp_url.as_deref())
        .map_err(|e| PluginError::Fetch(format!("Browser 端点配置错误: {e}")))?;
    Ok(resolved.is_some())
}

fn record_merged_cookie_token(
    store: &Store,
    ports: &StoreHostPorts,
    site: &Site,
    initial_obs: &store::SiteObservation,
    merged_cookie: Option<String>,
) -> Option<String> {
    let cookie = merged_cookie?;
    let after_obs = store.observe_site(site.id).ok().flatten()?;
    if !after_obs.site().enabled || after_obs.credential_epoch() != initial_obs.credential_epoch() {
        return None;
    }
    let origin = net_guard::normalize_url(&site.url, true).ok()?.origin;
    Some(ports.record_captured_cookies(CapturedCookieRecord {
        site_id: site.id,
        credential_epoch: initial_obs.credential_epoch().storage_key().to_string(),
        origin,
        set_cookies: Vec::new(),
        merged_cookie: Some(cookie),
    }))
}

fn classify_plugin_error(
    err: PluginError,
    action: MaintenanceAction,
    browser_attempted: bool,
) -> Result<MaintenanceOutcome, IndexerError> {
    let msg = err.to_string();
    match err {
        PluginError::UnknownOutcome(_) => {
            if action == MaintenanceAction::CheckIn {
                return Err(IndexerError::UnknownOutcome(msg));
            }
            return Err(IndexerError::Fetch(msg));
        }
        PluginError::BrowserPreNavigation(_) => return Err(IndexerError::BrowserSetup(msg)),
        _ => {}
    }
    let lower = msg.to_ascii_lowercase();
    if msg.contains("安全验证")
        || msg.contains("验证码")
        || msg.contains("登录表单")
        || lower.contains("challenge")
        || lower.contains("captcha")
    {
        return Ok(MaintenanceOutcome {
            state: MaintenanceState::Challenge,
            evidence: msg,
            session_capture: None,
        });
    }
    if msg.contains("Cookie") || msg.contains("登录状态") || msg.contains("Login required") {
        return Ok(MaintenanceOutcome {
            state: MaintenanceState::AuthExpired,
            evidence: msg,
            session_capture: None,
        });
    }
    if browser_attempted && action == MaintenanceAction::CheckIn {
        return Err(IndexerError::UnknownOutcome(msg));
    }
    Err(IndexerError::Fetch(msg))
}

impl BuiltinNexusPhpMaintenanceProvider {
    fn ensure_live_authority_and_site(
        &self,
        site: &Site,
    ) -> Result<store::SiteObservation, IndexerError> {
        let expected_gen = match &self.descriptor.source {
            ProviderSource::PluginWasm {
                plugin_generation, ..
            } => *plugin_generation,
            _ => 0,
        };
        let live_plugin = self
            .store
            .get_plugin(BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID)
            .map_err(|e| IndexerError::Fetch(e.to_string()))?
            .ok_or_else(|| IndexerError::Fetch("maintenance plugin missing".into()))?;
        if live_plugin.lifecycle != store::plugins::PluginLifecycle::Enabled
            || !live_plugin.desired_enabled
            || u64::try_from(live_plugin.generation).ok() != Some(expected_gen)
        {
            return Err(IndexerError::Fetch(
                "maintenance plugin is no longer enabled at expected generation".into(),
            ));
        }
        let initial_obs = self
            .store
            .observe_site(site.id)
            .map_err(|e| IndexerError::Fetch(e.to_string()))?
            .ok_or_else(|| IndexerError::Fetch("maintenance target site missing".into()))?;
        if !initial_obs.site().enabled {
            return Err(IndexerError::Fetch(
                "maintenance target site is disabled".into(),
            ));
        }
        let grants = self
            .store
            .get_plugin_grants(BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID)
            .map_err(|e| IndexerError::Fetch(e.to_string()))?;
        super::ports::ensure_provider_role_grant(
            plugin_api::GuestRole::Maintenance,
            &grants,
            Some(site.id),
        )
        .map_err(|e| IndexerError::Fetch(e.to_string()))?;
        Ok(initial_obs)
    }
}

impl MaintenanceProvider for BuiltinNexusPhpMaintenanceProvider {
    fn descriptor(&self) -> &MaintenanceDescriptor {
        &self.descriptor
    }

    fn run(
        &self,
        ctx: &MaintenanceCallContext<'_>,
        site: &Site,
        action: MaintenanceAction,
        _job_id: &str,
    ) -> Result<MaintenanceOutcome, IndexerError> {
        let initial_obs = self.ensure_live_authority_and_site(site)?;
        let login_css = self.indexer.upgrade().and_then(|idx| {
            idx.snapshot()
                .profile(&site.profile_id)
                .and_then(|p| p.login_success_css.clone())
        });
        let policy = NexusPhpPolicy::new(login_css);
        let bus = HookPipeline::new();
        let creds = CapturedSiteCredentials::new(site.clone());

        let mut fell_back_to_http = false;
        let result: Result<(), PluginError> = match action {
            MaintenanceAction::VerifySession => {
                if site.cookie.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    Err(PluginError::Fetch("请人工登录并配置有效 Cookie".into()))
                } else {
                    let via_http = || {
                        let req = FetchRequest {
                            key: format!("verify:{}", site.id),
                            url: site.url.clone(),
                            method: FetchMethod::Get,
                            body: None,
                            cookie: site.cookie.clone(),
                            api_key: site.api_key.clone(),
                            proxy: site.proxy.clone(),
                            cdp_url: site.cdp_url.clone(),
                        };
                        let body = ctx
                            .fetcher
                            .fetch(site, &req)
                            .map_err(|e| PluginError::Fetch(e.to_string()))?;
                        policy.authenticated(site, &body)
                    };
                    match site.maintenance_via {
                        MaintenanceVia::Http => via_http(),
                        MaintenanceVia::Browser => {
                            crate::site_maintenance::run_with_browser_fallback(
                                site,
                                "verify-session",
                                browser_endpoint_resolves(&self.browser, site),
                                || {
                                    LoginPlugin::browser(&bus, &creds, &self.browser)
                                        .with_policy(&policy)
                                        .login(site.id)
                                },
                                |_| {
                                    fell_back_to_http = true;
                                    via_http()
                                },
                            )
                        }
                    }
                }
            }
            MaintenanceAction::CheckIn => {
                let (override_policy, self_listen) = self.ports.site_http_policy_parts();
                let http = crate::check_in::attendance_http_from_parts(
                    override_policy.as_ref(),
                    self_listen,
                    site,
                );
                let via_http = |fallback: bool| {
                    let plugin = CheckInPlugin::http(&bus, &creds, &http).with_policy(&policy);
                    if fallback {
                        plugin.without_gate()
                    } else {
                        plugin
                    }
                    .check_in(site.id)
                };
                match site.maintenance_via {
                    MaintenanceVia::Http => via_http(false),
                    MaintenanceVia::Browser => crate::site_maintenance::run_with_browser_fallback(
                        site,
                        "check_in",
                        browser_endpoint_resolves(&self.browser, site),
                        || {
                            CheckInPlugin::browser(&bus, &creds, &self.browser)
                                .with_policy(&policy)
                                .check_in(site.id)
                        },
                        |fb| {
                            fell_back_to_http = true;
                            via_http(fb)
                        },
                    ),
                }
            }
        };

        let browser_attempted =
            site.maintenance_via == MaintenanceVia::Browser && !fell_back_to_http;
        match result {
            Ok(()) => {
                let capture = record_merged_cookie_token(
                    &self.store,
                    &self.ports,
                    site,
                    &initial_obs,
                    creds.take_cookie(),
                );
                Ok(MaintenanceOutcome {
                    state: MaintenanceState::Completed,
                    evidence: action.as_str().into(),
                    session_capture: capture,
                })
            }
            Err(err) => classify_plugin_error(err, action, browser_attempted),
        }
    }
}
