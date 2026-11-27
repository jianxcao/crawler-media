use std::time::{Duration, Instant};

use domain::SiteId;
use plugin_api::{BrowserPurpose, PluginTrust};
use serde_json::json;
use store::plugins::{
    AcceptPluginOperation, PluginCommandCause, PluginError, PluginGrant, PluginLifecycle,
    PluginRecord, PluginSubject, PluginVersionMetadata,
};

use super::wasm_runner::resolve_plugin_trust;
use super::{OperationAdminView, PluginService, PluginServiceError};
use crate::management::ApiState;

pub(crate) struct RunBrowserCheckInput {
    pub actor_user_id: domain::UserId,
    pub site_id: SiteId,
    pub purpose: BrowserPurpose,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: store::plugins::CanonicalPluginRequest,
}

struct LoadedBrowserCheck {
    plugin: PluginRecord,
    version: Option<PluginVersionMetadata>,
    grants: Vec<PluginGrant>,
    settings_cdp_enabled: bool,
    obs: store::SiteObservation,
}

fn load_cdp_enabled_from_app(app: &store::App<'_>) -> Result<bool, PluginServiceError> {
    match app.get_setting(crate::settings_keys::CDP_ENABLED)? {
        None => Ok(false),
        Some(raw) if raw.trim().is_empty() => Ok(false),
        Some(raw) => crate::config::parse_bool(&raw).ok_or_else(|| {
            PluginServiceError::invalid("plugin.invalid_input", "invalid Browser enabled flag")
        }),
    }
}

fn is_replayable_browser_check_conflict(err: &PluginServiceError) -> bool {
    match err {
        PluginServiceError::Conflict { code, .. } => *code != "instance.read_only",
        PluginServiceError::Store(store::StoreError::Plugin(PluginError::Conflict)) => true,
        _ => false,
    }
}

struct BrowserProbeInputs {
    plugin_enabled: bool,
    trust_ok: bool,
    proxy_conflict: bool,
    settings_cdp_enabled: bool,
    grant_present: bool,
    site_id: SiteId,
    purpose: BrowserPurpose,
}

fn claim_browser_check_in_app_tx(
    app: &store::App<'_>,
    pid: &str,
    actor: domain::UserId,
    expected_rev: i64,
    site_id: SiteId,
    idem: String,
    req_canon: store::plugins::CanonicalPluginRequest,
) -> Result<
    Result<(LoadedBrowserCheck, store::plugins::PluginCommandClaim), OperationAdminView>,
    PluginServiceError,
> {
    if let Some(op) = app.get_plugin_operation_by_idempotency(
        &PluginSubject::User(actor),
        Some(pid),
        &idem,
        &req_canon,
        "browser-check",
    )? {
        return Ok(Err(super::idempotency::load_app_operation_admin_view(
            app,
            op.operation_id,
        )?));
    }
    let plugin = app
        .get_plugin(pid)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
    if plugin.revision != expected_rev {
        return Err(PluginServiceError::conflict(
            "plugin.revision_conflict",
            "plugin revision mismatch",
        ));
    }
    let version = app.get_plugin_version(&plugin.plugin_id, &plugin.package_hash)?;
    let grants = app.get_plugin_grants(&plugin.plugin_id)?;
    let settings_cdp_enabled = load_cdp_enabled_from_app(app)?;
    let obs = app
        .observe_site(site_id)?
        .ok_or_else(|| PluginServiceError::not_found("site.not_found", "site not found"))?;
    let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
        plugin_id: Some(plugin.plugin_id.clone()),
        subject: PluginSubject::User(actor),
        idempotency_key: idem,
        request: req_canon,
        kind: "browser-check".into(),
        expected_revision: Some(expected_rev),
        expected_generation: None,
        cause: PluginCommandCause::root(format!("browser-check:{}", plugin.plugin_id)),
    })?;
    let op_id = accepted.operation.operation_id;
    if accepted.replayed {
        return Ok(Err(super::idempotency::load_app_operation_admin_view(
            app, op_id,
        )?));
    }
    let claim = app.claim_plugin_command(
        op_id,
        accepted.operation.revision,
        None,
        0,
        super::reauth::now_unix(),
    )?;
    Ok(Ok((
        LoadedBrowserCheck {
            plugin,
            version,
            grants,
            settings_cdp_enabled,
            obs,
        },
        claim,
    )))
}

impl PluginService {
    pub(crate) async fn run_browser_check(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: RunBrowserCheckInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        super::admin_ops::require_foreground_write_admitted(state)?;
        if let Some(replayed) = super::lifecycle_ops::check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "browser-check",
        )
        .await?
        {
            return Ok(replayed);
        }

        let pid2 = plugin_id.to_string();
        let expected_rev = input.expected_revision;
        let site_id = input.site_id;
        let purpose = input.purpose;
        let actor = input.actor_user_id;
        let idem = input.idempotency_key.clone();
        let req_canon = input.request.clone();

        let tx_res = super::config_ops::run_app_write_tx(state, move |app| {
            claim_browser_check_in_app_tx(app, &pid2, actor, expected_rev, site_id, idem, req_canon)
        })
        .await;

        let (loaded, claim) = match tx_res {
            Ok(Ok(pair)) => pair,
            Ok(Err(replayed)) => return Ok(replayed),
            Err(err) => {
                if is_replayable_browser_check_conflict(&err)
                    && let Some(replayed) =
                        super::lifecycle_ops::check_management_replay_after_write(
                            state,
                            input.actor_user_id,
                            Some(plugin_id),
                            &input.idempotency_key,
                            &input.request,
                            "browser-check",
                        )
                        .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };

        let probe_inputs = build_browser_probe_inputs(&loaded, state, site_id, purpose);
        let site_cdp_url = loaded.obs.site().cdp_url.clone();
        let s = state.clone();
        let op_id = claim.operation_id();

        super::service::run_blocking(move || {
            let (ok, phase, detail) =
                execute_browser_probe(&s, &probe_inputs, site_cdp_url.as_deref());
            let result_bytes = serde_json::to_vec(&detail).ok();
            super::op_settlement::complete_non_staged_op_with_result(
                &s.store,
                &claim,
                ok,
                phase,
                result_bytes,
            )?;
            super::lifecycle_ops::load_operation_admin_view(&s.store, op_id)
        })
        .await
    }
}

fn build_browser_probe_inputs(
    loaded: &LoadedBrowserCheck,
    state: &ApiState,
    site_id: SiteId,
    purpose: BrowserPurpose,
) -> BrowserProbeInputs {
    let plugin_enabled =
        loaded.plugin.lifecycle == PluginLifecycle::Enabled && loaded.plugin.desired_enabled;
    let sig_status = loaded
        .version
        .as_ref()
        .map_or("", |v| v.signature_status.as_str());
    let trust = resolve_plugin_trust(
        &loaded.plugin.plugin_id,
        sig_status,
        &state.admin_trusted_plugins,
    );
    let trust_ok = matches!(trust, PluginTrust::BuiltIn | PluginTrust::AdminTrusted);
    let proxy_conflict = loaded
        .obs
        .site()
        .proxy
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    let cap = match purpose {
        BrowserPurpose::Search => "browser:search",
        BrowserPurpose::Maintenance => "browser:maintenance",
    };
    let grant_present = loaded.grants.iter().any(|g| {
        g.capability == cap
            && super::command_targets::grant_scopes_cover_site(
                std::slice::from_ref(&g.scope),
                site_id,
            )
    });
    BrowserProbeInputs {
        plugin_enabled,
        trust_ok,
        proxy_conflict,
        settings_cdp_enabled: loaded.settings_cdp_enabled,
        grant_present,
        site_id,
        purpose,
    }
}

fn execute_browser_probe(
    state: &ApiState,
    probe: &BrowserProbeInputs,
    site_cdp_url: Option<&str>,
) -> (bool, &'static str, serde_json::Value) {
    if !probe.plugin_enabled {
        return (
            false,
            "plugin-not-enabled",
            json!({"ok": false, "reason": "plugin_not_enabled"}),
        );
    }
    if !probe.trust_ok {
        return (
            false,
            "browser-isolation-unavailable",
            json!({"ok": false, "reason": "browser_isolation_unavailable"}),
        );
    }
    if probe.proxy_conflict {
        return (
            false,
            "site-proxy-conflict",
            json!({"ok": false, "reason": "site_proxy_conflict"}),
        );
    }
    if !(probe.settings_cdp_enabled && state.browser.cdp_enabled().unwrap_or(false)) {
        return (
            false,
            "cdp-disabled",
            json!({"ok": false, "reason": "cdp_disabled"}),
        );
    }
    if !probe.grant_present {
        return (
            false,
            "missing-grant",
            json!({"ok": false, "reason": "missing_grant"}),
        );
    }
    match state
        .browser
        .check_plugin_cdp_context(site_cdp_url, Instant::now() + Duration::from_secs(10))
    {
        Ok(()) => (
            true,
            "checked",
            json!({
                "ok": true,
                "site_id": probe.site_id.to_string(),
                "purpose": match probe.purpose {
                    BrowserPurpose::Search => "search",
                    BrowserPurpose::Maintenance => "maintenance",
                },
                "blank_context_disposed": true
            }),
        ),
        Err(err) => (
            false,
            "cdp-check-failed",
            json!({"ok": false, "error_kind": format!("{:?}", err.kind)}),
        ),
    }
}
