//! Sites: configured PT trackers. Credentials plaintext in SQLite
//! (ADR-0003). Boost (刷流) config lives in settings KV per site.

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
    #[serde(default)]
    downloader_id: Option<String>,
    #[serde(default)]
    maintenance_via: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct BoostInput {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    budget_bytes: Option<u64>,
    #[serde(default)]
    hold_days: Option<u32>,
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

fn auth_type(site: &Site) -> &'static str {
    if site.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        "apikey"
    } else if site.cookie.as_deref().is_some_and(|c| !c.is_empty()) {
        "cookie"
    } else {
        "none"
    }
}

mod boost;
mod maintenance;
mod read;
mod write;

pub(crate) use boost::*;
pub(crate) use maintenance::*;
pub(crate) use read::*;
pub(crate) use write::*;

pub(crate) async fn site_catalog(State(state): State<ApiState>) -> Response {
    // Profiles are compile-time + overlay; expose id/name only.
    // demo 是测试桩，不列进站点配置。
    let items: Vec<Value> = state
        .indexer
        .profile_ids()
        .into_iter()
        .filter(|id| id != "demo")
        .map(|id| {
            json!({
                "profile_id": id,
                "auth_types": if id == "mteam" { ["apikey"] } else { ["cookie"] },
            })
        })
        .collect();
    ok(Value::Array(items)).into_response()
}
