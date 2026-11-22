use domain::{Site, SiteId};
use hooks::{Bus, HttpPost, NexusPhpPolicy, PluginError, SiteCredentials};
use serde_json::{Value, json};

use crate::Store;
use crate::management::ApiState;

pub fn run(state: &ApiState) -> Result<(), String> {
    let sites = state
        .store
        .list_enabled_sites()
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "【站点签到】开始执行周期性签到与保活任务，已启用站点数量: {}",
        sites.len()
    );
    let bus = Bus::new();
    let creds = StoreSites(&state.store);
    let _http = AttendanceHttp;
    let mut failures = Vec::new();
    for site in sites {
        // 判断站点类型：纯 API 架构站点（如新版 M-Team）无签到端点，无需也不支持打卡保活；
        // 但具备 api_check_in_path（如 HDArea /api/v1/user/signin）或使用 Cookie 模式的混合站点正常执行签到。
        if let Some(profile) = state.indexer.profile(&site.profile_id)
            && profile.framework == indexer::Framework::Api
            && !uses_api_check_in(profile, &site)
            && !uses_cookie_maintenance(profile, &site)
        {
            tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】API 令牌架构站点，跳过页面打卡签到");
            continue;
        }

        tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】正在执行站点签到/保活请求");
        let result = crate::site_maintenance::perform_check_in(state, &site, &bus, &creds);
        match result {
            Ok(_) => {
                tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】签到/保活成功");
            }
            Err(error) => {
                tracing::error!(site = %site.name, site_id = %site.id, error = %error, "【站点签到】签到/保活失败");
                let title = format!("站点签到失败：{}", site.name);
                let content = error.to_string();
                crate::worker::notify_public(state, &title, &content);
                failures.push(format!("{}: {error}", site.name));
            }
        }
    }
    tracing::info!("【站点签到】本轮签到检查完成，正在进行刷流配额检查");
    boost_check(state)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("站点签到失败: {}", failures.join("; ")))
    }
}

/// 刷流检查：每个启用 boost 且有 budget 的站点，若当前上传量达到预算
/// 则在统计里标 upper_limit_reached（按上游刷流语义，后续可暂停）。
fn boost_check(state: &ApiState) -> Result<(), String> {
    let category = state
        .store
        .default_downloader()
        .ok()
        .flatten()
        .and_then(|d| d.category)
        .unwrap_or_default();
    let uploaded = state
        .downloader
        .uploaded_by_category(&category)
        .unwrap_or(0);
    let store = state.store.as_ref();
    let sites = store.list_enabled_sites().map_err(|e| e.to_string())?;
    for site in sites {
        let key = format!("boost.{}", site.id);
        let Some(raw) = store.get_setting(&key).ok().flatten() else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if config["enabled"] != true {
            continue;
        }
        let budget = config["budget_bytes"].as_u64().unwrap_or(0);
        let reached = budget > 0 && uploaded >= budget;
        let mut next = config.clone();
        next["upper_limit_reached"] = json!(reached);
        let _ = store.put_setting(&key, &next.to_string());
    }
    Ok(())
}

pub(crate) fn attendance_http() -> AttendanceHttp {
    AttendanceHttp
}

pub(crate) struct AttendanceHttp;

impl HttpPost for AttendanceHttp {
    fn post(
        &self,
        url: &str,
        body: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        attendance_request(url, Some(body), cookie, proxy)
    }

    fn get(
        &self,
        url: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        attendance_request(url, None, cookie, proxy)
    }
}

pub(crate) fn uses_api_check_in(profile: &indexer::Profile, site: &Site) -> bool {
    profile.api_check_in_path.is_some()
        && site
            .api_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty())
}

pub(crate) fn uses_cookie_maintenance(profile: &indexer::Profile, site: &Site) -> bool {
    match profile.framework {
        indexer::Framework::Nexusphp => true,
        indexer::Framework::Api => {
            site.api_key.as_deref().is_none_or(|k| k.trim().is_empty())
                && site.cookie.as_deref().is_some_and(|c| !c.trim().is_empty())
                && profile.effective_auth_types().contains(&"cookie")
        }
    }
}

pub(crate) fn site_policy(state: &ApiState, site: &Site) -> Result<NexusPhpPolicy, PluginError> {
    let profile = state.indexer.profile(&site.profile_id)
        .ok_or_else(|| {
            tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site maintenance profile unavailable");
            PluginError::Fetch("Site profile 不可用；请选择支持认证验证的模板".into())
        })?;
    if !uses_cookie_maintenance(profile, site) {
        tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site page maintenance unsupported for API profile");
        return Err(PluginError::Fetch(
            "API Site 不支持页面 Login/Check-in；请配置有效 API key".into(),
        ));
    }
    Ok(NexusPhpPolicy::new(profile.login_success_css.clone()))
}

pub(crate) fn ensure_check_in_supported(state: &ApiState, site: &Site) -> Result<(), PluginError> {
    let profile = state.indexer.profile(&site.profile_id).ok_or_else(|| {
        tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site maintenance profile unavailable");
        PluginError::Fetch("Site profile 不可用；请选择支持认证验证的模板".into())
    })?;
    if uses_cookie_maintenance(profile, site) || profile.api_check_in_path.is_some() {
        return Ok(());
    }
    tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site check-in unsupported for API profile");
    Err(PluginError::Fetch(
        "API Site 不支持页面 Login/Check-in；请配置有效 API key".into(),
    ))
}

pub(crate) fn perform_api_check_in(
    site: &Site,
    profile: &indexer::Profile,
) -> Result<(), PluginError> {
    let api_key = site
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| PluginError::Fetch("站点未配置有效的 API 密钥，无法执行 API 签到".into()))?;
    let raw_path = profile
        .api_check_in_path
        .as_deref()
        .ok_or_else(|| PluginError::Fetch("该站点模板不支持 API 签到".into()))?;
    let path = if raw_path.starts_with('/') {
        raw_path.to_string()
    } else {
        format!("/{raw_path}")
    };
    let base = profile.effective_site_base_url(&site.url);
    let url = format!("{base}{path}");
    let host = validate_non_placeholder_url(&url)?;
    let mut config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(10)));
    if let Some(proxy) = site.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        let proxy = ureq::Proxy::new(proxy)
            .map_err(|error| PluginError::Fetch(format!("invalid Site proxy: {error}")))?;
        config = config.proxy(Some(proxy));
    }
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .header("X-API-Key", api_key)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body("{}")
        .map_err(|error| PluginError::Fetch(error.to_string()))?;
    let started = std::time::Instant::now();
    let response = ureq::Agent::new_with_config(config.build())
        .run(request)
        .map_err(|error| {
            tracing::error!(site_host = %host, %error, "Site API check-in HTTP request failed");
            PluginError::Fetch(error.to_string())
        })?;
    let status = response.status();
    let text = response.into_body().read_to_string().map_err(|error| {
        tracing::error!(site_host = %host, %error, "Site API check-in response read failed");
        PluginError::Fetch(error.to_string())
    })?;
    tracing::debug!(
        site_host = %host,
        elapsed_ms = started.elapsed().as_millis(),
        status = status.as_u16(),
        "Site API check-in response received"
    );
    validate_api_check_in_response(status.as_u16(), status.is_success(), &text)
}

fn validate_non_placeholder_url(url: &str) -> Result<String, PluginError> {
    let parsed = url::Url::parse(url).map_err(|error| PluginError::Fetch(error.to_string()))?;
    let host = parsed.host_str().unwrap_or_default();
    if host.ends_with(".example")
        || host.ends_with(".invalid")
        || host.ends_with(".test")
        || ["example.com", "example.org", "example.net"].contains(&host)
    {
        tracing::error!(
            site_host = host,
            "Site maintenance rejected placeholder URL"
        );
        return Err(PluginError::Fetch(
            "Site URL 是占位地址；请配置实际站点地址与有效 Cookie 或 API 密钥".into(),
        ));
    }
    Ok(host.to_string())
}

fn validate_api_check_in_response(
    status_code: u16,
    is_2xx: bool,
    text: &str,
) -> Result<(), PluginError> {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) if is_2xx => {
            return Err(PluginError::Fetch(format!(
                "API 签到响应非合法 JSON（HTTP {status_code}）：{error}"
            )));
        }
        Err(_) => {
            let preview: String = text.trim().chars().take(160).collect();
            return Err(PluginError::Fetch(format!(
                "API 签到失败（HTTP {status_code}）：{preview}"
            )));
        }
    };
    let message = parsed
        .get("message")
        .or_else(|| parsed.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if is_2xx {
        let explicit_fail = parsed.get("success").and_then(Value::as_bool) == Some(false);
        let code_ok = !explicit_fail
            && match parsed.get("code") {
                Some(Value::Number(n)) => n.as_i64() == Some(0),
                Some(Value::String(s)) => s.trim() == "0",
                _ => false,
            };
        let code_nonzero = match parsed.get("code") {
            Some(Value::Number(n)) => n.as_i64() != Some(0),
            Some(Value::String(s)) => s.trim() != "0",
            _ => false,
        };
        let success_ok =
            !code_nonzero && parsed.get("success").and_then(Value::as_bool) == Some(true);
        if code_ok || success_ok || indicates_already_signed_in(message) {
            return Ok(());
        }
    }
    let detail = if message.is_empty() {
        text.trim().chars().take(160).collect::<String>()
    } else {
        message.to_string()
    };
    Err(PluginError::Fetch(format!(
        "API 签到失败（HTTP {status_code}）：{detail}"
    )))
}

fn indicates_already_signed_in(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    message.contains("已签到")
        || message.contains("已经签到")
        || message.contains("重复签到")
        || lower.contains("already signed")
        || lower.contains("already checked")
        || (lower.contains("already") && lower.contains("sign"))
}

fn attendance_request(
    url: &str,
    body: Option<&str>,
    cookie: Option<&str>,
    proxy: Option<&str>,
) -> Result<String, PluginError> {
    let host = validate_non_placeholder_url(url)?;
    let mut config =
        ureq::config::Config::builder().timeout_global(Some(std::time::Duration::from_secs(10)));
    if let Some(proxy) = proxy.filter(|proxy| !proxy.trim().is_empty()) {
        let proxy = ureq::Proxy::new(proxy)
            .map_err(|error| PluginError::Fetch(format!("invalid Site proxy: {error}")))?;
        config = config.proxy(Some(proxy));
    }
    let method = if body.is_some() { "POST" } else { "GET" };
    let mut request = ureq::http::Request::builder().method(method).uri(url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36");
    if body.is_some() {
        request = request.header("Content-Type", "application/x-www-form-urlencoded");
    }
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let request = request
        .body(body.unwrap_or_default())
        .map_err(|error| PluginError::Fetch(error.to_string()))?;
    let started = std::time::Instant::now();
    let response = ureq::Agent::new_with_config(config.build()).run(request).map_err(|error| {
        tracing::error!(site_host = %host, method, %error, "Site maintenance HTTP request failed");
        PluginError::Fetch(error.to_string())
    })?;
    tracing::debug!(
        site_host = %host,
        method,
        elapsed_ms = started.elapsed().as_millis(),
        status = response.status().as_u16(),
        "Site maintenance page fetched; business outcome still requires validation"
    );
    let cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| format!("Set-Cookie: {value}\n"))
        .collect();
    let text = response.into_body().read_to_string().map_err(|error| {
        tracing::error!(site_host = %host, method, %error, "Site maintenance response read failed");
        PluginError::Fetch(error.to_string())
    })?;
    Ok(format!("{}{text}", cookies.concat()))
}

pub(crate) struct StoreSites<'a>(&'a Store);

impl SiteCredentials for StoreSites<'_> {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        self.0
            .get_site(id)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
            .ok_or(PluginError::NotFound)
    }

    fn save(&self, site: &Site) -> Result<(), PluginError> {
        if self
            .0
            .save_site(site)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
        {
            Ok(())
        } else {
            Err(PluginError::NotFound)
        }
    }
}
