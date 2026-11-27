//! Sites: configured PT trackers. Credentials plaintext in SQLite (ADR-0003).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use domain::{Site, SiteId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::blocking_budget::BlockingError;
use crate::http::{err, ok};
use crate::management::ApiState;

/// 只读站点请求（验证/上传统计）的等待预算；不替代 adapter 自身的 HTTP 超时。
const SITE_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(12);

/// Blocking-budget failure → HTTP response（上游错误走各自原有路径）。
fn blocking_error(error: BlockingError, code: &'static str) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(StatusCode::GATEWAY_TIMEOUT, code, "站点请求超时"),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct SiteInput {
    name: String,
    #[serde(default)]
    url: String,
    profile_id: String,
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    rss_url: Option<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    cdp_url: Option<String>,
    #[serde(default)]
    downloader_id: Option<String>,
    #[serde(default)]
    maintenance_via: Option<String>,
    #[serde(default)]
    browser_search: Option<bool>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub(crate) struct PatchSiteInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    rss_url: Option<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    cdp_url: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    downloader_id: Option<Option<String>>,
    #[serde(default)]
    maintenance_via: Option<String>,
    #[serde(default)]
    browser_search: Option<bool>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// Cookie/API keys stay masked; URL fields are only exposed to administrators.
fn site_json(site: &Site) -> Value {
    json!({
        "id": site.id.to_string(),
        "name": site.name,
        "url": site.url,
        "profile_id": site.profile_id,
        "cookie": null,
        "api_key": null,
        "rss_url": site.rss_url,
        "proxy": site.proxy,
        "rate_limit_per_minute": site.rate_limit_per_minute,
        "cdp_url": site.cdp_url,
        "downloader_id": site.downloader_id.map(|id| id.to_string()),
        "enabled": site.enabled,
        "maintenance_via": site.maintenance_via.as_str(),
        "browser_search": site.browser_search,
        "timezone": site.timezone,
    })
}

fn site_public(site: &Site, auth_type: &str, admin: bool) -> Value {
    let mut v = site_json(site);
    v["auth_type"] = json!(auth_type);
    if !admin {
        // Secrets can occur in arbitrary path/query/fragment components, not just userinfo.
        v["url"] = json!("");
        for key in ["rss_url", "proxy", "cdp_url"] {
            v[key] = Value::Null;
        }
    }
    v
}

fn profile_auth_support(profile: Option<&indexer::Profile>) -> (bool, bool) {
    match profile.filter(|p| p.id != "demo") {
        Some(p) => {
            let types = p.effective_auth_types();
            (types.contains(&"apikey"), types.contains(&"cookie"))
        }
        None => (true, true),
    }
}

fn auth_type_for(site: &Site, profile: Option<&indexer::Profile>) -> &'static str {
    let (allow_api_key, allow_cookie) = profile_auth_support(profile);
    if allow_api_key && site.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        "apikey"
    } else if allow_cookie && site.cookie.as_deref().is_some_and(|c| !c.is_empty()) {
        "cookie"
    } else {
        "none"
    }
}

mod maintenance;
mod plugin_actions;
mod read;
mod write;

pub(crate) use maintenance::*;
pub(crate) use plugin_actions::*;
pub(crate) use read::*;
pub(crate) use write::*;

pub(crate) async fn site_catalog(State(state): State<ApiState>) -> Response {
    let snapshot = state.indexer.snapshot();
    let items: Vec<Value> = snapshot
        .indexer_descriptors()
        .filter(|desc| !desc.hidden_from_catalog)
        .map(|desc| {
            let auth_types: Vec<&'static str> =
                desc.auth_types.iter().map(|a| a.as_str()).collect();
            let capabilities: Vec<&'static str> =
                desc.capabilities.iter().map(|c| c.as_str()).collect();
            let has_plugin_check_in = desc.source.plugin_id().is_some_and(|pid| {
                snapshot.maintenance_entries().any(|m| {
                    m.descriptor().source.plugin_id() == Some(pid)
                        && m.descriptor().availability.is_enabled()
                        && m.descriptor()
                            .actions
                            .contains(&indexer::MaintenanceAction::CheckIn)
                })
            });
            let supports_check_in = snapshot
                .profile(&desc.id)
                .is_some_and(|profile| profile.supports_check_in())
                || has_plugin_check_in;
            json!({
                "profile_id": desc.id,
                "name": desc.name,
                "kind": desc.kind,
                "auth_types": auth_types,
                "supports_check_in": supports_check_in,
                "capabilities": capabilities,
                "source": desc.source.as_label(),
                "availability": match &desc.availability {
                    indexer::ProviderAvailability::Enabled => json!({ "status": "enabled" }),
                    indexer::ProviderAvailability::Disabled { reason } => json!({
                        "status": "disabled",
                        "reason": reason,
                    }),
                },
            })
        })
        .collect();
    ok(Value::Array(items)).into_response()
}
