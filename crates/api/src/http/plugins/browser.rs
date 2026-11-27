use std::str::FromStr;

use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::{MaintenanceVia, SiteId, UserId};
use plugin_api::{BrowserPurpose, PluginTrust};
use serde::Deserialize;
use serde_json::json;
use store::plugins::PluginLifecycle;

use super::extractors::CanonicalWrite;
use super::{accepted_op, ok_json, service_error_response};
use crate::http::err;
use crate::http::store_wait::store_read;
use crate::management::ApiState;
use crate::plugins::{PluginServiceError, RunBrowserCheckInput, resolve_plugin_trust};
use crate::runtime_browser::BrowserSettings;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserAvailabilityQuery {
    pub site_id: String,
    pub purpose: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserCheckRequest {
    pub site_id: String,
    pub purpose: String,
}

fn parse_purpose(raw: &str) -> Result<BrowserPurpose, Response> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "search" => Ok(BrowserPurpose::Search),
        "maintenance" => Ok(BrowserPurpose::Maintenance),
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "purpose must be `search` or `maintenance`",
        )),
    }
}

fn parse_site_id(raw: &str) -> Result<SiteId, Response> {
    SiteId::from_str(raw.trim()).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "invalid site_id",
        )
    })
}

fn trust_label(trust: PluginTrust) -> &'static str {
    match trust {
        PluginTrust::BuiltIn => "builtin",
        PluginTrust::AdminTrusted => "admin_trusted",
        PluginTrust::Untrusted => "untrusted",
    }
}

pub(super) async fn browser_availability(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<BrowserAvailabilityQuery>,
) -> Response {
    if crate::plugins::has_attribution_headers(&headers)
        && let Err(err) = state
            .plugins()
            .verify_plugin_attribution(&state, &id, &headers)
            .await
    {
        return service_error_response(err);
    }
    let site_id = match parse_site_id(&query.site_id) {
        Ok(sid) => sid,
        Err(resp) => return resp,
    };
    let purpose = match parse_purpose(&query.purpose) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let admin_trusted = state.admin_trusted_plugins.clone();
    let browser_cdp_enabled = state.browser.cdp_enabled().unwrap_or(false);

    let result = store_read(&state, &[store::Db::App], move |store: &store::Store| {
        read_browser_availability(
            store,
            &id,
            site_id,
            purpose,
            &admin_trusted,
            browser_cdp_enabled,
        )
    })
    .await;

    match result {
        Ok(Ok(val)) => ok_json(StatusCode::OK, &val),
        Ok(Err(e)) => service_error_response(e),
        Err(err) => err.response(),
    }
}

fn read_browser_availability(
    store: &store::Store,
    id: &str,
    site_id: SiteId,
    purpose: BrowserPurpose,
    admin_trusted: &std::collections::HashSet<String>,
    browser_cdp_enabled: bool,
) -> Result<serde_json::Value, PluginServiceError> {
    let Some(plugin) = store.get_plugin(id).map_err(PluginServiceError::from)? else {
        return Err(PluginServiceError::not_found(
            "plugin.missing",
            "plugin not found",
        ));
    };
    let version = store
        .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        .map_err(PluginServiceError::from)?;
    let grants = store
        .get_plugin_grants(&plugin.plugin_id)
        .map_err(PluginServiceError::from)?;
    let Some(obs) = store
        .observe_site(site_id)
        .map_err(PluginServiceError::from)?
    else {
        return Err(PluginServiceError::not_found(
            "site.not_found",
            "site not found",
        ));
    };
    let settings = BrowserSettings::load(store)
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;

    let sig_status = version.as_ref().map_or("", |v| v.signature_status.as_str());
    let trust = resolve_plugin_trust(&plugin.plugin_id, sig_status, admin_trusted);
    let trust_ok = matches!(trust, PluginTrust::BuiltIn | PluginTrust::AdminTrusted);
    let cdp_enabled = settings.cdp_enabled() && browser_cdp_enabled;
    let site_enabled = match purpose {
        BrowserPurpose::Search => obs.site().browser_search,
        BrowserPurpose::Maintenance => obs.site().maintenance_via == MaintenanceVia::Browser,
    };
    let proxy_conflict = obs
        .site()
        .proxy
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    let cap = match purpose {
        BrowserPurpose::Search => "browser:search",
        BrowserPurpose::Maintenance => "browser:maintenance",
    };
    let grant_present = grants.iter().any(|g| {
        g.capability == cap
            && crate::plugins::command_targets::grant_scopes_cover_site(
                std::slice::from_ref(&g.scope),
                site_id,
            )
    });
    let plugin_enabled = plugin.lifecycle == PluginLifecycle::Enabled && plugin.desired_enabled;

    let mut reasons = Vec::new();
    if !plugin_enabled {
        reasons.push("plugin_not_enabled");
    }
    if !trust_ok {
        reasons.push("browser_isolation_unavailable");
    }
    if !cdp_enabled {
        reasons.push("cdp_disabled");
    }
    if !site_enabled {
        reasons.push("site_browser_disabled");
    }
    if proxy_conflict {
        reasons.push("site_proxy_conflict");
    }
    if !grant_present {
        reasons.push("missing_grant");
    }
    let available = reasons.is_empty();
    Ok(json!({
        "available": available,
        "plugin_id": plugin.plugin_id,
        "site_id": site_id.to_string(),
        "purpose": match purpose {
            BrowserPurpose::Search => "search",
            BrowserPurpose::Maintenance => "maintenance",
        },
        "trust": trust_label(trust),
        "cdp_enabled": cdp_enabled,
        "site_enabled": site_enabled,
        "grant_present": grant_present,
        "status": if available { "ready" } else { "unavailable" },
        "missing_requirements": &reasons,
        "reasons": reasons,
    }))
}

pub(super) async fn run_browser_check(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path(id): Path<String>,
    w: CanonicalWrite<BrowserCheckRequest>,
) -> Response {
    if crate::plugins::has_attribution_headers(&headers) {
        if let Err(err) = state
            .plugins()
            .verify_plugin_attribution(&state, &id, &headers)
            .await
        {
            return service_error_response(err);
        }
        return service_error_response(PluginServiceError::forbidden(
            "plugin.capability_denied",
            "plugin UI surfaces cannot run browser-checks",
        ));
    }
    let site_id = match parse_site_id(&w.body.site_id) {
        Ok(sid) => sid,
        Err(resp) => return resp,
    };
    let purpose = match parse_purpose(&w.body.purpose) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match state
        .plugins()
        .run_browser_check(
            &state,
            &id,
            RunBrowserCheckInput {
                actor_user_id: user_id,
                site_id,
                purpose,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}
