mod effect_step;

use std::sync::Arc;

use domain::{MaintenanceVia, Site, SiteId};
use hooks::{HookPipeline, LoginPlugin, PluginError, SiteCredentials};
use indexer::{
    Framework, IndexerError, MaintenanceAction, MaintenanceCallContext, MaintenanceProvider,
    MaintenanceState,
};
use store::{
    ClaimSiteMaintenanceReceipt, FinalizeSiteMaintenanceReceipt, SiteMaintenanceAction,
    SiteMaintenanceClaimResult, SiteMaintenanceReceipt, SiteMaintenanceState, SiteObservation,
};

use crate::management::ApiState;
use crate::plugins::builtins::{
    BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID, BUILTIN_NEXUSPHP_MAINTENANCE_PROVIDER_ID,
    BuiltinNexusPhpMaintenanceProvider,
};

pub(crate) use effect_step::{execute_effect_step, parse_payload_site_id};

#[derive(Debug, thiserror::Error)]
pub enum MaintenanceError {
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    InvalidTimezone(String),
    #[error("站点维护处于未知状态，请核查后重试 ({0})")]
    BlockedUnknown(String),
    #[error("站点维护正在进行中，请稍后重试")]
    InFlight,
    #[error("{0}")]
    Unknown(String),
    #[error("{0}")]
    Failed(String),
}

impl MaintenanceError {
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}

pub struct MaintenanceExecutionResult {
    pub receipt: SiteMaintenanceReceipt,
    pub deduplicated: bool,
}

/// Whether a browser endpoint resolves for this site. Configuration errors are
/// surfaced (not swallowed); `Ok(false)` means CDP is disabled / no endpoint.
fn browser_endpoint_resolves(state: &ApiState, site: &Site) -> Result<bool, PluginError> {
    let config = state
        .browser
        .effective_config()
        .map_err(|e| PluginError::Fetch(format!("Browser 配置错误: {e}")))?;
    let resolved = config
        .resolve(site.cdp_url.as_deref())
        .map_err(|e| PluginError::Fetch(format!("Browser 端点配置错误: {e}")))?;
    Ok(resolved.is_some())
}

/// Run a browser-routed maintenance step. Only failures that happened before
/// navigation (`BrowserPreNavigation`) fall back to HTTP; anything after the
/// page was navigated is returned as-is so the action is never submitted twice.
pub(crate) fn run_with_browser_fallback(
    site: &Site,
    action: &str,
    endpoint_resolves: Result<bool, PluginError>,
    browser: impl FnOnce() -> Result<(), PluginError>,
    http: impl FnOnce(bool) -> Result<(), PluginError>,
) -> Result<(), PluginError> {
    if !endpoint_resolves? {
        tracing::warn!(site_id = %site.id, site_name = %site.name, action,
            "Browser CDP disabled or no endpoint, falling back to HTTP");
        return http(false);
    }
    tracing::info!(site_id = %site.id, site_name = %site.name, action, "Attempting browser maintenance");
    match browser() {
        Ok(()) => Ok(()),
        Err(error) if error.is_browser_pre_navigation() => {
            tracing::warn!(site_id = %site.id, site_name = %site.name, action, %error,
                "Browser failed before navigation, falling back to HTTP");
            // The browser attempt already emitted the hook event.
            http(true)
        }
        Err(error) => {
            tracing::error!(site_id = %site.id, site_name = %site.name, action, %error,
                "Browser failed after navigation; not falling back to HTTP");
            Err(error)
        }
    }
}

fn resolve_explicit_maintenance_provider(
    state: &ApiState,
    snapshot: &indexer::RegistrySnapshot,
    site: &Site,
    action: MaintenanceAction,
    pid: &str,
) -> Result<(String, i64, Arc<dyn MaintenanceProvider>), MaintenanceError> {
    let entry = snapshot
        .maintenance(pid)
        .ok_or_else(|| MaintenanceError::Unsupported(format!("未知维护 provider `{pid}`")))?;
    let desc = entry.descriptor();
    if !desc.availability.is_enabled() {
        return Err(MaintenanceError::Unsupported(format!(
            "维护 provider `{pid}` 已停用"
        )));
    }
    if !desc.actions.contains(&action) {
        return Err(MaintenanceError::Unsupported(format!(
            "维护 provider `{pid}` 不支持动作 `{}`",
            action.as_str()
        )));
    }
    let plugin_id = desc
        .source
        .plugin_id()
        .unwrap_or(BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID)
        .to_string();
    let plugin_gen = state
        .store
        .get_plugin(&plugin_id)
        .ok()
        .flatten()
        .map_or(1, |p| p.generation);
    let provider: Arc<dyn MaintenanceProvider> = if pid == BUILTIN_NEXUSPHP_MAINTENANCE_PROVIDER_ID
    {
        let Some(profile) = snapshot.profile(&site.profile_id) else {
            return Err(MaintenanceError::Unsupported(
                "Site profile 不可用；请选择支持认证验证的模板".into(),
            ));
        };
        if !crate::check_in::uses_cookie_maintenance(&profile, site) {
            return Err(MaintenanceError::Unsupported(
                "API Site 不支持页面 Login/Check-in；请配置有效 API key".into(),
            ));
        }
        Arc::new(BuiltinNexusPhpMaintenanceProvider::new(
            u64::try_from(plugin_gen).unwrap_or(1),
            Arc::downgrade(&state.indexer),
            state.browser.clone(),
            state.store.clone(),
            state.plugins.ports().clone(),
        ))
    } else {
        entry.provider_arc()
    };
    Ok((plugin_id, plugin_gen, provider))
}

pub(crate) fn resolve_maintenance_provider(
    state: &ApiState,
    site: &Site,
    action: MaintenanceAction,
    explicit_provider_id: Option<&str>,
) -> Result<(String, i64, Arc<dyn MaintenanceProvider>), MaintenanceError> {
    let snapshot = state.indexer.snapshot();
    if let Some(pid) = explicit_provider_id {
        return resolve_explicit_maintenance_provider(state, &snapshot, site, action, pid);
    }

    if let Ok((plugin_id, _)) = indexer::parse_plugin_provider_id(&site.profile_id) {
        for entry in snapshot.maintenance_entries() {
            let desc = entry.descriptor();
            if desc.source.plugin_id() == Some(plugin_id)
                && desc.availability.is_enabled()
                && desc.actions.contains(&action)
            {
                let plugin_gen = state
                    .store
                    .get_plugin(plugin_id)
                    .ok()
                    .flatten()
                    .map_or(1, |p| p.generation);
                return Ok((plugin_id.to_string(), plugin_gen, entry.provider_arc()));
            }
        }
    }

    let profile = snapshot.profile(&site.profile_id).ok_or_else(|| {
        MaintenanceError::Unsupported("Site profile 不可用；请选择支持认证验证的模板".into())
    })?;
    if !crate::check_in::uses_cookie_maintenance(&profile, site) {
        if action == MaintenanceAction::CheckIn
            && profile.framework == Framework::Api
            && profile.api_check_in_path.is_some()
        {
            return Err(MaintenanceError::Failed(
                "站点未配置有效的 API 密钥或 Cookie，无法执行签到".into(),
            ));
        }
        return Err(MaintenanceError::Unsupported(
            "API Site 不支持页面 Login/Check-in；请配置有效 API key".into(),
        ));
    }

    if let Some(plugin) = state
        .store
        .get_plugin(BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID)
        .ok()
        .flatten()
        && plugin.lifecycle != store::plugins::PluginLifecycle::Enabled
    {
        return Err(MaintenanceError::Unsupported(
            "内置 NexusPHP 维护插件已停用".into(),
        ));
    }

    let plugin_gen = state
        .store
        .get_plugin(BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID)
        .ok()
        .flatten()
        .map_or(1, |p| p.generation);
    let provider: Arc<dyn MaintenanceProvider> = Arc::new(BuiltinNexusPhpMaintenanceProvider::new(
        u64::try_from(plugin_gen).unwrap_or(1),
        Arc::downgrade(&state.indexer),
        state.browser.clone(),
        state.store.clone(),
        state.plugins.ports().clone(),
    ));
    Ok((
        BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID.to_string(),
        plugin_gen,
        provider,
    ))
}

fn to_store_action(action: MaintenanceAction) -> SiteMaintenanceAction {
    match action {
        MaintenanceAction::VerifySession => SiteMaintenanceAction::VerifySession,
        MaintenanceAction::CheckIn => SiteMaintenanceAction::CheckIn,
    }
}

pub fn execute_receipt_maintenance(
    state: &ApiState,
    observed: &SiteObservation,
    action: MaintenanceAction,
    explicit_provider_id: Option<&str>,
    now_utc: i64,
    job_id: &str,
) -> Result<MaintenanceExecutionResult, MaintenanceError> {
    execute_receipt_maintenance_inner(
        state,
        observed,
        action,
        explicit_provider_id,
        now_utc,
        job_id,
        false,
    )
    .unwrap_or_else(|_| {
        Err(MaintenanceError::Failed(
            "Site maintenance write admission closed".into(),
        ))
    })
}

pub(crate) fn execute_foreground_receipt_maintenance(
    state: &ApiState,
    observed: &SiteObservation,
    action: MaintenanceAction,
    explicit_provider_id: Option<&str>,
    now_utc: i64,
    job_id: &str,
) -> Result<Result<MaintenanceExecutionResult, MaintenanceError>, axum::response::Response> {
    execute_receipt_maintenance_inner(
        state,
        observed,
        action,
        explicit_provider_id,
        now_utc,
        job_id,
        true,
    )
}

fn with_maintenance_store<T>(
    state: &ApiState,
    foreground: bool,
    work: impl FnOnce(&crate::Store) -> T,
) -> Result<T, axum::response::Response> {
    if foreground {
        crate::http::store_wait::with_foreground_store(state, work)
    } else {
        Ok(work(&state.store))
    }
}

fn execute_receipt_maintenance_inner(
    state: &ApiState,
    observed: &SiteObservation,
    action: MaintenanceAction,
    explicit_provider_id: Option<&str>,
    now_utc: i64,
    job_id: &str,
    foreground: bool,
) -> Result<Result<MaintenanceExecutionResult, MaintenanceError>, axum::response::Response> {
    let site = observed.site().clone();
    let (plugin_id, generation, provider) =
        match resolve_maintenance_provider(state, &site, action, explicit_provider_id) {
            Ok(triple) => triple,
            Err(err) => return Ok(Err(err)),
        };
    let provider_id = provider.descriptor().id.clone();

    let window = match crate::maintenance_window::compute_window(&site.timezone, now_utc) {
        Ok(w) => w,
        Err(err) => return Ok(Err(MaintenanceError::InvalidTimezone(err))),
    };

    let claim_input = ClaimSiteMaintenanceReceipt {
        site_id: site.id,
        action: to_store_action(action),
        window_key: window.window_key,
        timezone: window.timezone,
        window_start_utc: window.window_start_utc,
        window_end_utc: window.window_end_utc,
        now_utc,
        plugin_id,
        provider_id,
        generation,
        credential_epoch: observed.credential_epoch().storage_key().to_string(),
    };
    let claim = match with_maintenance_store(state, foreground, |s| {
        s.claim_site_maintenance_receipt(&claim_input)
    })? {
        Ok(c) => c,
        Err(e) => return Ok(Err(MaintenanceError::Failed(e.to_string()))),
    };

    let receipt = match claim {
        SiteMaintenanceClaimResult::AlreadyCompleted(r) => {
            tracing::info!(
                site_id = %site.id,
                action = action.as_str(),
                window_key = %r.window_key,
                "Site maintenance deduplicated via existing completed receipt"
            );
            return Ok(Ok(MaintenanceExecutionResult {
                receipt: r,
                deduplicated: true,
            }));
        }
        SiteMaintenanceClaimResult::BlockedUnknown(r) => {
            tracing::warn!(
                site_id = %site.id,
                action = action.as_str(),
                receipt_id = %r.receipt_id,
                "Site maintenance blocked by unresolved unknown receipt"
            );
            return Ok(Err(MaintenanceError::BlockedUnknown(r.receipt_id)));
        }
        SiteMaintenanceClaimResult::InFlight(_) => {
            return Ok(Err(MaintenanceError::InFlight));
        }
        SiteMaintenanceClaimResult::Claimed(r) => r,
    };

    let ctx = MaintenanceCallContext {
        generation: u64::try_from(generation).unwrap_or(1),
        fetcher: state.indexer.as_ref(),
    };
    let run_res = provider.run(&ctx, &site, action, job_id);
    let done_now = store::unix_now();
    finalize_maintenance_run(
        state, observed, action, receipt, run_res, done_now, foreground,
    )
}

fn apply_captured_cookies(
    state: &ApiState,
    observed: &SiteObservation,
    session_capture: Option<&str>,
    foreground: bool,
) -> Result<(), axum::response::Response> {
    let site = observed.site();
    let Some(token) = session_capture else {
        return Ok(());
    };
    let Some(record) = state.plugins.ports().take_captured_cookies(token) else {
        return Ok(());
    };
    if record.site_id != site.id
        || record.credential_epoch != observed.credential_epoch().storage_key()
        || !net_guard::normalize_url(&site.url, true).is_ok_and(|u| u.origin == record.origin)
    {
        return Ok(());
    }
    let merged = record
        .merged_cookie
        .or_else(|| hooks::merge_set_cookies(site.cookie.as_deref(), &record.set_cookies));
    if let Some(cookie) = merged {
        match with_maintenance_store(state, foreground, |s| {
            s.save_site_if_current(observed, Some(&cookie))
        })? {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    site_id = %site.id,
                    "Site changed during maintenance cookie CAS; preserving manual credentials and completed receipt"
                );
            }
            Err(e) => {
                tracing::warn!(site_id = %site.id, error = %e, "Failed saving captured maintenance cookie");
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn persist_maintenance_receipt_with_event(
    state: &ApiState,
    site_id: SiteId,
    action: MaintenanceAction,
    receipt: &SiteMaintenanceReceipt,
    outcome_state: &str,
    receipt_state: SiteMaintenanceState,
    outcome_tag: &str,
    evidence: String,
    done_now: i64,
    foreground: bool,
) -> Result<Result<SiteMaintenanceReceipt, store::StoreError>, axum::response::Response> {
    let outbox = (action == MaintenanceAction::CheckIn)
        .then(|| {
            crate::plugins::events::build_site_checkin_after_event(
                site_id,
                &receipt.window_key,
                outcome_state,
                done_now.saturating_mul(1_000),
            )
        })
        .flatten();
    let finalize_input = FinalizeSiteMaintenanceReceipt {
        receipt_id: receipt.receipt_id.clone(),
        expected_revision: receipt.revision,
        state: receipt_state,
        outcome: Some(outcome_tag.to_string()),
        evidence: Some(evidence),
        now_utc: done_now,
    };
    let updated = with_maintenance_store(state, foreground, |s| {
        s.finalize_site_maintenance_receipt_with_outbox(&finalize_input, outbox.as_ref())
    })?;
    if updated.is_ok() && outbox.is_some() {
        crate::plugins::events::wake(state);
    }
    Ok(updated)
}

fn finalize_maintenance_run(
    state: &ApiState,
    observed: &SiteObservation,
    action: MaintenanceAction,
    receipt: SiteMaintenanceReceipt,
    run_res: Result<indexer::MaintenanceOutcome, IndexerError>,
    done_now: i64,
    foreground: bool,
) -> Result<Result<MaintenanceExecutionResult, MaintenanceError>, axum::response::Response> {
    let site_id = observed.site().id;
    let mark = |o_state: &str, r_state: SiteMaintenanceState, tag: &str, ev: String| {
        persist_maintenance_receipt_with_event(
            state, site_id, action, &receipt, o_state, r_state, tag, ev, done_now, foreground,
        )
    };
    let mark_logged = |o_state: &str, r_state: SiteMaintenanceState, tag: &str, ev: String| {
        if let Err(err) = mark(o_state, r_state, tag, ev)? {
            tracing::warn!(
                site_id = %site_id,
                action = action.as_str(),
                receipt_id = %receipt.receipt_id,
                outcome = tag,
                error = %err,
                "Failed finalizing site maintenance receipt"
            );
        }
        Ok::<(), axum::response::Response>(())
    };
    let clean_ev = |s: &str| {
        if s.is_empty() {
            String::new()
        } else {
            hooks::sanitize_reason(s)
        }
    };
    match run_res {
        Ok(outcome) => match outcome.state {
            MaintenanceState::Completed | MaintenanceState::AlreadyCompleted => {
                apply_captured_cookies(
                    state,
                    observed,
                    outcome.session_capture.as_deref(),
                    foreground,
                )?;
                let (o_state, tag) = if outcome.state == MaintenanceState::AlreadyCompleted {
                    ("already_completed", "already-completed")
                } else {
                    ("completed", "completed")
                };
                let updated = match mark(
                    o_state,
                    SiteMaintenanceState::Completed,
                    tag,
                    clean_ev(&outcome.evidence),
                )? {
                    Ok(u) => u,
                    Err(e) => {
                        return Ok(Err(MaintenanceError::Failed(hooks::sanitize_reason(
                            &e.to_string(),
                        ))));
                    }
                };
                Ok(Ok(MaintenanceExecutionResult {
                    receipt: updated,
                    deduplicated: false,
                }))
            }
            MaintenanceState::AuthExpired | MaintenanceState::Challenge => {
                let tag = if outcome.state == MaintenanceState::AuthExpired {
                    "auth-expired"
                } else {
                    "challenge"
                };
                let ev = clean_ev(&outcome.evidence);
                mark_logged("failed", SiteMaintenanceState::Failed, tag, ev.clone())?;
                Ok(Err(MaintenanceError::Failed(ev)))
            }
            MaintenanceState::Unknown => {
                let ev = clean_ev(&outcome.evidence);
                mark_logged(
                    "unknown",
                    SiteMaintenanceState::Unknown,
                    "unknown",
                    ev.clone(),
                )?;
                Ok(Err(MaintenanceError::Unknown(ev)))
            }
        },
        Err(IndexerError::UnknownOutcome(msg)) => {
            let clean = hooks::sanitize_reason(&msg);
            mark_logged(
                "unknown",
                SiteMaintenanceState::Unknown,
                "unknown",
                clean.clone(),
            )?;
            Ok(Err(MaintenanceError::Unknown(clean)))
        }
        Err(err) => {
            let msg = hooks::sanitize_reason(&err.to_string());
            mark_logged("failed", SiteMaintenanceState::Failed, "error", msg.clone())?;
            Ok(Err(MaintenanceError::Failed(msg)))
        }
    }
}

pub(super) fn try_perform_api_check_in(
    state: &ApiState,
    observed: &SiteObservation,
) -> Option<Result<MaintenanceExecutionResult, MaintenanceError>> {
    let site = observed.site();
    let profile = state.indexer.profile(&site.profile_id)?;
    if !crate::check_in::uses_api_check_in(&profile, site) {
        return None;
    }
    let creds = crate::check_in::StoreSites::new(state, observed.clone());
    let current = match creds.load(site.id) {
        Ok(s) => s,
        Err(err) => {
            return Some(Err(MaintenanceError::Failed(hooks::sanitize_reason(
                &err.to_string(),
            ))));
        }
    };
    let now_utc = store::unix_now();
    let window = match crate::maintenance_window::compute_window(&current.timezone, now_utc) {
        Ok(w) => w,
        Err(err) => return Some(Err(MaintenanceError::InvalidTimezone(err))),
    };
    Some(
        crate::check_in::perform_api_check_in(state, &current, &profile)
            .map(|()| MaintenanceExecutionResult {
                receipt: SiteMaintenanceReceipt {
                    receipt_id: format!("api-checkin:{}:{now_utc}", current.id),
                    site_id: current.id,
                    action: SiteMaintenanceAction::CheckIn,
                    window_key: window.window_key,
                    timezone: window.timezone,
                    window_start_utc: window.window_start_utc,
                    window_end_utc: window.window_end_utc,
                    plugin_id: "native.api".into(),
                    provider_id: current.profile_id.clone(),
                    generation: 1,
                    credential_epoch: observed.credential_epoch().storage_key().to_string(),
                    state: SiteMaintenanceState::Completed,
                    outcome: Some("completed".into()),
                    evidence: Some("api-check-in".into()),
                    revision: 1,
                    created_at: now_utc,
                    updated_at: now_utc,
                },
                deduplicated: false,
            })
            .map_err(|err| MaintenanceError::Failed(hooks::sanitize_reason(&err.to_string()))),
    )
}

pub(crate) fn run_scheduled_check_in_for_site(
    state: &ApiState,
    observed: SiteObservation,
) -> Result<(), String> {
    if let Some(res) = try_perform_api_check_in(state, &observed) {
        return res.map(|_| ()).map_err(|e| e.to_string());
    }
    let site = observed.site().clone();
    match resolve_maintenance_provider(state, &site, MaintenanceAction::CheckIn, None) {
        Ok(_) => {}
        Err(MaintenanceError::Unsupported(reason)) => {
            tracing::info!(site = %site.name, site_id = %site.id, %reason, "【站点签到】无可用维护 provider，跳过签到");
            return Ok(());
        }
        Err(err) => return Err(err.to_string()),
    }
    let now = store::unix_now();
    execute_receipt_maintenance(
        state,
        &observed,
        MaintenanceAction::CheckIn,
        None,
        now,
        &format!("check-in:{}", site.id),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

pub fn perform_login(
    state: &ApiState,
    site: &Site,
    bus: &HookPipeline,
    creds: &dyn SiteCredentials,
) -> Result<(), PluginError> {
    // Login never inserts into site_maintenance_receipts (CHECK action IN ('verify-session','check-in')).
    let policy = crate::check_in::site_policy(state, site)?;
    let http = crate::check_in::attendance_http_for_state(state, site);
    let via_http = |fallback: bool| {
        let plugin = LoginPlugin::http(bus, creds, &http).with_policy(&policy);
        if fallback {
            plugin.without_gate()
        } else {
            plugin
        }
        .login(site.id)
    };
    match site.maintenance_via {
        MaintenanceVia::Http => via_http(false),
        MaintenanceVia::Browser => run_with_browser_fallback(
            site,
            "login",
            browser_endpoint_resolves(state, site),
            || {
                LoginPlugin::browser(bus, creds, &state.browser)
                    .with_policy(&policy)
                    .login(site.id)
            },
            via_http,
        ),
    }
}

pub fn perform_check_in(
    state: &ApiState,
    observed: &SiteObservation,
) -> Result<MaintenanceExecutionResult, MaintenanceError> {
    if let Some(res) = try_perform_api_check_in(state, observed) {
        return res;
    }
    execute_receipt_maintenance(
        state,
        observed,
        MaintenanceAction::CheckIn,
        None,
        store::unix_now(),
        &format!("check-in:{}", observed.site().id),
    )
}

pub(crate) fn perform_foreground_check_in(
    state: &ApiState,
    observed: &SiteObservation,
) -> Result<Result<MaintenanceExecutionResult, MaintenanceError>, axum::response::Response> {
    if let Some(res) = try_perform_api_check_in(state, observed) {
        return Ok(res);
    }
    execute_foreground_receipt_maintenance(
        state,
        observed,
        MaintenanceAction::CheckIn,
        None,
        store::unix_now(),
        &format!("check-in:{}", observed.site().id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn site() -> Site {
        Site {
            id: domain::SiteId::new(),
            name: "t".into(),
            url: "https://example.org/".into(),
            profile_id: "nexusphp".into(),
            cookie: Some("a=b".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Browser,
            browser_search: false,
            timezone: "UTC".into(),
        }
    }

    fn run(
        resolves: Result<bool, PluginError>,
        browser: Result<(), PluginError>,
    ) -> (Result<(), PluginError>, u32) {
        let http_calls = Cell::new(0u32);
        let browser_was_pre_nav = matches!(browser, Err(PluginError::BrowserPreNavigation(_)));
        let s = site();
        let r = run_with_browser_fallback(
            &s,
            "check_in",
            resolves,
            || browser,
            |fallback| {
                assert_eq!(fallback, browser_was_pre_nav);
                http_calls.set(http_calls.get() + 1);
                Ok(())
            },
        );
        (r, http_calls.get())
    }

    #[test]
    fn post_navigation_error_does_not_fall_back_no_double_check_in() {
        let (r, calls) = run(Ok(true), Err(PluginError::Fetch("after goto".into())));
        assert!(r.is_err());
        assert_eq!(
            calls, 0,
            "post-navigation failure must not trigger HTTP check-in"
        );
        let (r, calls) = run(Ok(true), Err(PluginError::Veto("policy".into())));
        assert!(r.is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn pre_navigation_error_falls_back_once() {
        let (r, calls) = run(
            Ok(true),
            Err(PluginError::BrowserPreNavigation("busy".into())),
        );
        assert!(r.is_ok());
        assert_eq!(calls, 1);
    }

    #[test]
    fn browser_success_does_not_call_http() {
        let (r, calls) = run(Ok(true), Ok(()));
        assert!(r.is_ok());
        assert_eq!(calls, 0);
    }

    #[test]
    fn disabled_endpoint_uses_http_and_config_error_surfaces() {
        let (r, calls) = run(Ok(false), Ok(()));
        assert!(r.is_ok());
        assert_eq!(calls, 1);
        let (r, calls) = run(Err(PluginError::Fetch("bad config".into())), Ok(()));
        assert!(r.is_err());
        assert_eq!(
            calls, 0,
            "config errors must not be swallowed into HTTP fallback"
        );
    }
}
