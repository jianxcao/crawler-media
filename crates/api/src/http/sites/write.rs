//! Site write paths: create / replace / patch / delete plus the public facade.

use super::*;

async fn create_site_inner(State(state): State<ApiState>, Json(body): Json<SiteInput>) -> Response {
    if state.indexer.profile(&body.profile_id).is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "site.unknown_profile",
            &format!("未知的站点模板 {}", body.profile_id),
        );
    }
    let profile = state.indexer.profile(&body.profile_id);
    let url = if body.url.trim().is_empty() {
        match profile.and_then(|p| p.base_url.clone()) {
            Some(base) if !base.is_empty() => base,
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid",
                    "该站点模板未内置基底地址，站点 URL 不能为空",
                );
            }
        }
    } else {
        body.url
    };
    let downloader_id = body
        .downloader_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .and_then(|id| domain::DownloaderId::from_str(id).ok());
    let maintenance_via = match body.maintenance_via.as_deref() {
        Some(s) => match s.parse::<domain::MaintenanceVia>() {
            Ok(v) => v,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_maintenance_via",
                    "无效的 maintenance_via",
                );
            }
        },
        None => domain::MaintenanceVia::Http,
    };
    if maintenance_via == domain::MaintenanceVia::Browser
        && body.proxy.as_deref().is_some_and(|p| !p.trim().is_empty())
    {
        return err(
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        );
    }
    if let Some(cdp_url) = body.cdp_url.as_deref().filter(|u| !u.trim().is_empty())
        && let Err(e) = indexer::BrowserConfig::validate_endpoint(cdp_url.trim()) {
            return err(
                StatusCode::BAD_REQUEST,
                "site.invalid_endpoint",
                &e.to_string(),
            );
        }
    let site = Site {
        id: SiteId::new(),
        name: body.name,
        url,
        profile_id: body.profile_id,
        cookie: body.cookie,
        api_key: body.api_key,
        rss_url: body.rss_url,
        proxy: body.proxy,
        rate_limit_per_minute: body.rate_limit_per_minute,
        cdp_url: body.cdp_url,
        downloader_id,
        enabled: body.enabled,
        maintenance_via,
    };
    let store = state.store.lock();
    if let Err(error) = store.insert_site(&site) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    let mut view = site_public(&site, auth_type(&site), true);
    view["cookie"] = site
        .cookie
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    view["api_key"] = site
        .api_key
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    (StatusCode::CREATED, ok(view)).into_response()
}

async fn put_site_inner(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<SiteInput>,
) -> Response {
    if state.indexer.profile(&body.profile_id).is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "site.invalid",
            &format!("未知站点配置: {}", body.profile_id),
        );
    }
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let downloader_id = match body
        .downloader_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(domain::DownloaderId::from_str)
        .transpose()
    {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "下载器 id 无效"),
    };
    let store = state.store.lock();
    let current = match store.get_site(site_id) {
        Ok(Some(site)) => site,
        Ok(None) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
        Err(error) => return site_store_error(&error),
    };
    let site = match replacement_site(current, body, downloader_id) {
        Ok(site) => site,
        Err((status, code, msg)) => return err(status, code, msg),
    };
    let updated = match store.save_site(&site) {
        Ok(updated) => updated,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    if !updated {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    }
    ok(site_json(&site)).into_response()
}

/// Null/absent masked credentials mean unchanged; an explicit empty string clears them.
fn replacement_site(
    current: Site,
    body: SiteInput,
    downloader_id: Option<domain::DownloaderId>,
) -> Result<Site, (StatusCode, &'static str, &'static str)> {
    let maintenance_via = match body.maintenance_via.as_deref() {
        Some(s) => match s.parse::<domain::MaintenanceVia>() {
            Ok(v) => v,
            Err(_) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "site.invalid_maintenance_via",
                    "无效的 maintenance_via",
                ));
            }
        },
        None => current.maintenance_via,
    };
    let proxy = replacement_credential(body.proxy, current.proxy);
    if maintenance_via == domain::MaintenanceVia::Browser
        && proxy.as_deref().is_some_and(|p| !p.trim().is_empty())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        ));
    }
    let cdp_url = replacement_credential(body.cdp_url, current.cdp_url);
    if let Some(endpoint) = cdp_url.as_deref().filter(|u| !u.trim().is_empty())
        && let Err(_) = indexer::BrowserConfig::validate_endpoint(endpoint.trim()) {
            return Err((
                StatusCode::BAD_REQUEST,
                "site.invalid_endpoint",
                "无效的 CDP discovery 端点",
            ));
        }
    Ok(Site {
        id: current.id,
        name: body.name,
        url: body.url,
        profile_id: body.profile_id,
        cookie: replacement_credential(body.cookie, current.cookie),
        api_key: replacement_credential(body.api_key, current.api_key),
        rss_url: replacement_credential(body.rss_url, current.rss_url),
        proxy,
        rate_limit_per_minute: body.rate_limit_per_minute,
        cdp_url,
        downloader_id,
        enabled: body.enabled,
        maintenance_via,
    })
}

fn replacement_credential(input: Option<String>, current: Option<String>) -> Option<String> {
    match input {
        Some(value) => (!value.is_empty()).then_some(value),
        None => current,
    }
}

fn site_store_error(error: &impl std::fmt::Display) -> Response {
    tracing::error!(%error, "Site storage operation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

async fn patch_site_inner(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<PatchSiteInput>,
) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    let Some(mut site) = store.get_site(id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    if let Some(name) = body.name.filter(|n| !n.trim().is_empty()) {
        site.name = name;
    }
    if let Some(url) = body.url.filter(|u| !u.trim().is_empty()) {
        site.url = url;
    }
    if let Some(cookie) = body.cookie {
        site.cookie = (!cookie.is_empty()).then_some(cookie);
    }
    if let Some(api_key) = body.api_key {
        site.api_key = (!api_key.is_empty()).then_some(api_key);
    }
    if let Some(rss_url) = body.rss_url {
        site.rss_url = (!rss_url.is_empty()).then_some(rss_url);
    }
    if let Some(proxy) = body.proxy {
        site.proxy = (!proxy.is_empty()).then_some(proxy);
    }
    if let Some(rate) = body.rate_limit_per_minute {
        site.rate_limit_per_minute = (rate > 0).then_some(rate);
    }
    if let Some(cdp_url) = body.cdp_url {
        let trimmed = cdp_url.trim();
        if !trimmed.is_empty()
            && let Err(_) = indexer::BrowserConfig::validate_endpoint(trimmed) {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_endpoint",
                    "无效的 CDP discovery 端点",
                );
            }
        site.cdp_url = (!cdp_url.is_empty()).then_some(cdp_url);
    }
    if let Some(downloader_id) = body.downloader_id {
        site.downloader_id = (!downloader_id.is_empty())
            .then(|| domain::DownloaderId::from_str(&downloader_id).ok())
            .flatten();
    }
    if let Some(mv_str) = body.maintenance_via {
        match mv_str.parse::<domain::MaintenanceVia>() {
            Ok(v) => site.maintenance_via = v,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_maintenance_via",
                    "无效的 maintenance_via",
                );
            }
        }
    }
    if let Some(enabled) = body.enabled {
        site.enabled = enabled;
    }
    if site.maintenance_via == domain::MaintenanceVia::Browser
        && site.proxy.as_deref().is_some_and(|p| !p.trim().is_empty())
    {
        return err(
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        );
    }
    if let Err(error) = store.save_site(&site) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(site_public(&site, auth_type(&site), true)).into_response()
}

async fn delete_site_inner(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    match store.delete_site(id) {
        Ok(true) => {
            tracing::info!(site_id = %id, "已删除索引站点");
            ok(json!({ "deleted": true })).into_response()
        }
        _ => {
            tracing::warn!(site_id = %id, "待删除的站点不存在");
            err(StatusCode::NOT_FOUND, "site.missing", "站点不存在")
        }
    }
}

pub(crate) async fn create_site(
    State(state): State<ApiState>,
    Json(body): Json<SiteInput>,
) -> Response {
    after_site_write(
        &state,
        create_site_inner(State(state.clone()), Json(body)).await,
    )
}

pub(crate) async fn put_site(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<SiteInput>,
) -> Response {
    after_site_write(
        &state,
        put_site_inner(State(state.clone()), Path(id), Json(body)).await,
    )
}

pub(crate) async fn patch_site(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<PatchSiteInput>,
) -> Response {
    after_site_write(
        &state,
        patch_site_inner(State(state.clone()), Path(id), Json(body)).await,
    )
}

pub(crate) async fn delete_site(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    after_site_write(
        &state,
        delete_site_inner(State(state.clone()), Path(id)).await,
    )
}

/// Site writes can change `managed_browser_sites`; re-run the managed-daemon
/// decision in the background after any successful write.
fn after_site_write(state: &ApiState, response: Response) -> Response {
    if response.status().is_success() {
        crate::http::browser_settings::spawn_reconcile(state);
    }
    response
}
