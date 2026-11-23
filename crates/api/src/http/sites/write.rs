//! Site write paths: create / replace / patch / delete plus the public facade.

use super::*;

fn normalize_optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn resolve_site_url(
    raw_url: &str,
    profile: Option<&indexer::Profile>,
) -> Result<String, (StatusCode, &'static str, &'static str)> {
    if !raw_url.trim().is_empty() {
        return Ok(raw_url.to_string());
    }
    match profile.and_then(|p| p.base_url.as_deref()).map(str::trim) {
        Some(base) if !base.is_empty() => Ok(base.to_string()),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "site.invalid",
            "该站点模板未内置基底地址，站点 URL 不能为空",
        )),
    }
}

async fn create_site_inner(State(state): State<ApiState>, Json(body): Json<SiteInput>) -> Response {
    let Some(profile) = state.indexer.profile(&body.profile_id) else {
        return err(
            StatusCode::BAD_REQUEST,
            "site.unknown_profile",
            &format!("未知的站点模板 {}", body.profile_id),
        );
    };
    let url = match resolve_site_url(&body.url, Some(profile)) {
        Ok(url) => url,
        Err((status, code, msg)) => return err(status, code, msg),
    };
    let downloader_id = body
        .downloader_id
        .as_deref()
        .map(str::trim)
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
    let proxy = body.proxy.and_then(normalize_optional);
    if maintenance_via == domain::MaintenanceVia::Browser && proxy.is_some() {
        return err(
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        );
    }
    let cdp_url = body.cdp_url.and_then(normalize_optional);
    if let Some(ref endpoint) = cdp_url
        && let Err(e) = indexer::BrowserConfig::validate_endpoint(endpoint)
    {
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
        cookie: body.cookie.and_then(normalize_optional),
        api_key: body.api_key.and_then(normalize_optional),
        rss_url: body.rss_url.and_then(normalize_optional),
        proxy,
        rate_limit_per_minute: body.rate_limit_per_minute,
        cdp_url,
        downloader_id,
        enabled: body.enabled,
        maintenance_via,
    };
    crate::http::store_wait::store_write(&state, move |store| {
        if let Err(error) = store.insert_site(&site) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        let view = site_public(&site, auth_type(&site), true);
        (StatusCode::CREATED, ok(view)).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

async fn put_site_inner(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<SiteInput>,
) -> Response {
    let Some(profile) = state.indexer.profile(&body.profile_id) else {
        return err(
            StatusCode::BAD_REQUEST,
            "site.invalid",
            &format!("未知站点配置: {}", body.profile_id),
        );
    };
    let resolved_url = match resolve_site_url(&body.url, Some(profile)) {
        Ok(url) => url,
        Err((status, code, msg)) => return err(status, code, msg),
    };
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let downloader_id = match body
        .downloader_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(domain::DownloaderId::from_str)
        .transpose()
    {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "下载器 id 无效"),
    };
    crate::http::store_wait::store_write(&state, move |store| {
        let expected = match store.get_site(site_id) {
            Ok(Some(site)) => site,
            Ok(None) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
            Err(error) => return site_store_error(&error),
        };
        let site = match replacement_site(expected.clone(), body, resolved_url, downloader_id) {
            Ok(site) => site,
            Err((status, code, msg)) => return err(status, code, msg),
        };
        match store.save_site_if_current(&site, &expected) {
            Ok(true) => ok(site_public(&site, auth_type(&site), true)).into_response(),
            Ok(false) => site_changed(),
            Err(error) => site_store_error(&error),
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

/// Null/absent masked credentials mean unchanged; an explicit empty string clears them.
fn replacement_site(
    current: Site,
    body: SiteInput,
    url: String,
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
    if let Some(ref endpoint) = cdp_url
        && indexer::BrowserConfig::validate_endpoint(endpoint).is_err()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "site.invalid_endpoint",
            "无效的 CDP discovery 端点",
        ));
    }
    Ok(Site {
        id: current.id,
        name: body.name,
        url,
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
        Some(value) => normalize_optional(value),
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

fn site_changed() -> Response {
    err(
        StatusCode::CONFLICT,
        "site.changed",
        "站点已更改，请刷新后重试",
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
    crate::http::store_wait::store_write(&state, move |store| {
        let expected = match store.get_site(id) {
            Ok(Some(site)) => site,
            Ok(None) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
            Err(error) => return site_store_error(&error),
        };
        let mut site = expected.clone();
        if let Some(name) = body.name.filter(|n| !n.trim().is_empty()) {
            site.name = name;
        }
        if let Some(url) = body.url.filter(|u| !u.trim().is_empty()) {
            site.url = url;
        }
        if let Some(cookie) = body.cookie {
            site.cookie = normalize_optional(cookie);
        }
        if let Some(api_key) = body.api_key {
            site.api_key = normalize_optional(api_key);
        }
        if let Some(rss_url) = body.rss_url {
            site.rss_url = normalize_optional(rss_url);
        }
        if let Some(proxy) = body.proxy {
            site.proxy = normalize_optional(proxy);
        }
        if let Some(rate) = body.rate_limit_per_minute {
            site.rate_limit_per_minute = (rate > 0).then_some(rate);
        }
        if let Some(cdp_url) = body.cdp_url {
            let normalized = normalize_optional(cdp_url);
            if let Some(ref endpoint) = normalized
                && indexer::BrowserConfig::validate_endpoint(endpoint).is_err()
            {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_endpoint",
                    "无效的 CDP discovery 端点",
                );
            }
            site.cdp_url = normalized;
        }
        if let Some(downloader_id) = body.downloader_id {
            site.downloader_id = normalize_optional(downloader_id)
                .and_then(|trimmed| domain::DownloaderId::from_str(&trimmed).ok());
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
        match store.save_site_if_current(&site, &expected) {
            Ok(true) => ok(site_public(&site, auth_type(&site), true)).into_response(),
            Ok(false) => site_changed(),
            Err(error) => site_store_error(&error),
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

async fn delete_site_inner(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    crate::http::store_wait::store_write(&state, move |store| match store.delete_site(id) {
        Ok(true) => {
            tracing::info!(site_id = %id, "已删除索引站点");
            ok(json!({ "deleted": true })).into_response()
        }
        Ok(false) => {
            tracing::warn!(site_id = %id, "待删除的站点不存在");
            err(StatusCode::NOT_FOUND, "site.missing", "站点不存在")
        }
        Err(error) => site_store_error(&error),
    })
    .await
    .unwrap_or_else(|response| response)
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
