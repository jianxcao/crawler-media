use std::io::Read as _;
use std::net::SocketAddr;
use std::time::Duration;

use domain::{Site, SiteId};
use hooks::{HttpPost, NexusPhpPolicy, PluginError, SiteCredentials};
use net_guard::{
    NormalizedTarget, PinnedResolver, PinnedTcpConnector, SystemDnsResolver, TargetPolicy,
    resolve_and_pin,
};
use serde_json::Value;
use ureq::unversioned::transport::{Connector as _, RustlsConnector};

use crate::management::ApiState;

const MAX_CHECK_IN_BODY_BYTES: u64 = 5 * 1024 * 1024;

pub fn run(state: &ApiState) -> Result<(), String> {
    let sites = state
        .store
        .list_enabled_sites()
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "【站点签到】开始执行周期性签到与保活任务，已启用站点数量: {}",
        sites.len()
    );
    let mut failures = Vec::new();
    for site in sites {
        let Some(observed) = state
            .store
            .observe_site(site.id)
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        let site = observed.site().clone();
        if !site.enabled {
            continue;
        }

        tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】正在执行站点签到/保活请求");
        let result = crate::site_maintenance::run_scheduled_check_in_for_site(state, observed);
        match result {
            Ok(()) => {
                tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】签到/保活成功");
            }
            Err(error) => {
                let clean = hooks::sanitize_reason(&error);
                tracing::error!(site = %site.name, site_id = %site.id, error = %clean, "【站点签到】签到/保活失败");
                let title = format!("站点签到失败：{}", site.name);
                crate::worker::notify_public(state, &title, &clean);
                failures.push(format!("{}: {clean}", site.name));
            }
        }
    }
    tracing::info!("【站点签到】本轮签到检查完成");
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("站点签到失败: {}", failures.join("; ")))
    }
}

pub(crate) fn site_target_policy_from_parts(
    override_policy: Option<&TargetPolicy>,
    self_listen: Option<SocketAddr>,
    site: &Site,
    profile: Option<&indexer::Profile>,
) -> TargetPolicy {
    let mut policy = override_policy
        .cloned()
        .unwrap_or_else(|| TargetPolicy::production().with_benchmark_fake_ip_domains(true));
    if let Some(self_addr) = self_listen {
        policy = policy.with_blocked_self(self_addr);
    }
    if let Ok(norm) = net_guard::normalize_url(&site.url, true) {
        let (next, _) = crate::http_fetcher::attach_site_private_exception_for_origin(
            policy,
            &site.url,
            &norm.origin,
        );
        policy = next;
    }
    if let Some(prof) = profile {
        let base = prof.effective_site_base_url(&site.url);
        if let Ok(norm) = net_guard::normalize_url(&base, true) {
            let (next, _) = crate::http_fetcher::attach_site_private_exception_for_origin(
                policy,
                &base,
                &norm.origin,
            );
            policy = next;
        }
    }
    policy
}

pub(crate) fn site_target_policy(
    state: &ApiState,
    site: &Site,
    profile: Option<&indexer::Profile>,
) -> TargetPolicy {
    site_target_policy_from_parts(
        state.plugin_http_policy_override.as_ref(),
        state.self_listen,
        site,
        profile,
    )
}

pub(crate) fn attendance_http_for_state(state: &ApiState, site: &Site) -> AttendanceHttp {
    AttendanceHttp {
        policy: site_target_policy(state, site, None),
    }
}

pub(crate) fn attendance_http_from_parts(
    override_policy: Option<&TargetPolicy>,
    self_listen: Option<SocketAddr>,
    site: &Site,
) -> AttendanceHttp {
    AttendanceHttp {
        policy: site_target_policy_from_parts(override_policy, self_listen, site, None),
    }
}

pub(crate) struct AttendanceHttp {
    pub(super) policy: TargetPolicy,
}

impl HttpPost for AttendanceHttp {
    fn post(
        &self,
        url: &str,
        body: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        attendance_request(url, Some(body), cookie, proxy, &self.policy)
    }

    fn get(
        &self,
        url: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        attendance_request(url, None, cookie, proxy, &self.policy)
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
    let snapshot = state.indexer.snapshot();
    let profile = snapshot.profile(&site.profile_id).ok_or_else(|| {
        tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site maintenance profile unavailable");
        PluginError::Fetch("Site profile 不可用；请选择支持认证验证的模板".into())
    })?;
    if !uses_cookie_maintenance(&profile, site) {
        tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site page maintenance unsupported for API profile");
        return Err(PluginError::Fetch(
            "API Site 不支持页面 Login/Check-in；请配置有效 API key".into(),
        ));
    }
    Ok(NexusPhpPolicy::new(profile.login_success_css.clone()))
}

pub(crate) fn perform_api_check_in(
    state: &ApiState,
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
    let policy = site_target_policy(state, site, Some(profile));
    let mut target = policy
        .normalize_url(&url)
        .map_err(|e| PluginError::Fetch(hooks::sanitize_reason(&e.to_string())))?;
    policy
        .validate_target_for_method(&target, Some("POST"))
        .map_err(|e| PluginError::Fetch(hooks::sanitize_reason(&e.to_string())))?;
    let initial_origin = target.origin.clone();
    let mut same_origin_chain = true;
    let mut body_opt: Option<&str> = Some("{}");
    let started = std::time::Instant::now();

    for hop in 0..=5 {
        let is_post = body_opt.is_some();
        let method = if is_post { "POST" } else { "GET" };
        let (agent, uri) = build_guarded_hop_agent(&target, &policy, site.proxy.as_deref(), false)?;
        let mut req_builder = ureq::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "User-Agent",
                crate::http_fetcher::effective_site_user_agent(),
            )
            .header("Accept", "application/json");
        if is_post {
            req_builder = req_builder.header("Content-Type", "application/json");
        }
        if same_origin_chain && target.origin == initial_origin {
            req_builder = req_builder.header("X-API-Key", api_key);
        }
        let response =
            run_optional_body_request(&agent, req_builder, body_opt, false).map_err(|err| {
                tracing::error!(site_host = %host, %err, "Site API check-in HTTP request failed");
                err
            })?;
        let status = response.status();
        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            let next_target = resolve_guarded_redirect(
                &target,
                &policy,
                response.headers(),
                status.as_u16(),
                method,
                hop,
                false,
            )?;
            if matches!(status.as_u16(), 301..=303) {
                body_opt = None;
            }
            same_origin_chain &= next_target.origin == initial_origin;
            target = next_target;
            continue;
        }
        let text = read_bounded_string(response, false)?;
        tracing::debug!(
            site_host = %host,
            elapsed_ms = started.elapsed().as_millis(),
            status = status.as_u16(),
            "Site API check-in response received"
        );
        return validate_api_check_in_response(status.as_u16(), status.is_success(), &text);
    }
    Err(PluginError::Fetch("too many redirects".into()))
}

fn validate_non_placeholder_url(url: &str) -> Result<String, PluginError> {
    let parsed = url::Url::parse(url)
        .map_err(|error| PluginError::Fetch(hooks::sanitize_reason(&error.to_string())))?;
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
            let preview = hooks::sanitize_reason(&preview);
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
    let raw_detail = if message.is_empty() {
        text.trim().chars().take(160).collect::<String>()
    } else {
        message.to_string()
    };
    let detail = hooks::sanitize_reason(&raw_detail);
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

fn wrap_hop_err(msg: String, mutating_sent: bool) -> PluginError {
    let clean = hooks::sanitize_reason(&msg);
    if mutating_sent {
        PluginError::UnknownOutcome(clean)
    } else {
        PluginError::Fetch(clean)
    }
}

fn map_attendance_run_error(
    error: ureq::Error,
    is_mutation: bool,
    mutating_sent: bool,
) -> PluginError {
    let msg = hooks::sanitize_reason(&error.to_string());
    if mutating_sent {
        return PluginError::UnknownOutcome(msg);
    }
    if !is_mutation {
        return PluginError::Fetch(msg);
    }
    let is_pre_send = match &error {
        ureq::Error::BadUri(_)
        | ureq::Error::InvalidProxyUrl
        | ureq::Error::Http(_)
        | ureq::Error::RequireHttpsOnly(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Tls(_)
        | ureq::Error::Pem(_)
        | ureq::Error::Rustls(_)
        | ureq::Error::TlsRequired => true,
        ureq::Error::Timeout(ureq::Timeout::Resolve | ureq::Timeout::Connect) => true,
        ureq::Error::Io(io_err) => matches!(
            io_err.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::NetworkUnreachable
                | std::io::ErrorKind::HostUnreachable
                | std::io::ErrorKind::AddrNotAvailable
                | std::io::ErrorKind::AddrInUse
                | std::io::ErrorKind::PermissionDenied
        ),
        _ => false,
    };
    if is_pre_send {
        PluginError::Fetch(msg)
    } else {
        PluginError::UnknownOutcome(msg)
    }
}

pub(super) fn resolve_guarded_redirect(
    current: &NormalizedTarget,
    policy: &TargetPolicy,
    headers: &ureq::http::HeaderMap,
    status: u16,
    current_method: &str,
    hop: u8,
    mutating_sent: bool,
) -> Result<NormalizedTarget, PluginError> {
    if hop == 5 {
        return Err(wrap_hop_err("too many redirects".into(), mutating_sent));
    }
    let location = headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            wrap_hop_err(format!("redirect {status} missing Location"), mutating_sent)
        })?;
    let next = current
        .url
        .join(location)
        .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
    if !matches!(next.scheme(), "http" | "https") {
        return Err(wrap_hop_err(
            format!("unsupported redirect scheme `{}`", next.scheme()),
            mutating_sent,
        ));
    }
    if current.url.scheme() == "https" && next.scheme() == "http" {
        return Err(wrap_hop_err(
            "https to http downgrade redirect blocked".into(),
            mutating_sent,
        ));
    }
    let next_target = policy
        .normalize_url(next.as_str())
        .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
    let next_method = if matches!(status, 301..=303) {
        "GET"
    } else {
        current_method
    };
    policy
        .validate_redirect_hop_for_method(current, &next_target, Some(next_method))
        .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
    Ok(next_target)
}

fn handle_non_2xx_attendance_response(
    response: ureq::http::Response<ureq::Body>,
    status: u16,
    hop_host: &str,
    method: &str,
    is_mutation: bool,
    prior_mutating_sent: bool,
) -> PluginError {
    if prior_mutating_sent {
        return PluginError::UnknownOutcome(format!("http status: {status}"));
    }
    if is_mutation {
        if matches!(status, 401 | 403 | 503)
            && let Ok(body) = read_bounded_string(response, true)
            && let Err(challenge_err) = hooks::reject_challenge(&body)
        {
            return challenge_err;
        }
        return PluginError::UnknownOutcome(format!("http status: {status}"));
    }
    if matches!(status, 401 | 403 | 503)
        && let Ok(body) = read_bounded_string(response, false)
        && let Err(challenge_err) = hooks::reject_challenge(&body)
    {
        return challenge_err;
    }
    tracing::debug!(
        site_host = hop_host,
        method,
        status,
        "Site maintenance GET returned non-2xx status"
    );
    PluginError::Fetch(format!("http status: {status}"))
}

fn build_guarded_hop_agent(
    target: &NormalizedTarget,
    policy: &TargetPolicy,
    proxy: Option<&str>,
    mutating_sent: bool,
) -> Result<(ureq::Agent, ureq::http::Uri), PluginError> {
    let host = validate_non_placeholder_url(target.url.as_str()).map_err(|e| {
        if mutating_sent {
            PluginError::UnknownOutcome(e.to_string())
        } else {
            e
        }
    })?;
    let _ = host;
    let config = ureq::config::Config::builder()
        .max_redirects(0)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .http_status_as_error(false)
        .timeout_resolve(Some(Duration::from_secs(5)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(10)));
    if let Some(proxy) = proxy.filter(|p| !p.trim().is_empty()) {
        if let Some(ip) = target.host_is_ip {
            policy
                .validate_socket_addr(target, SocketAddr::new(ip, target.port))
                .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
        }
        let proxy = ureq::Proxy::new(proxy)
            .map_err(|e| wrap_hop_err(format!("invalid Site proxy: {e}"), mutating_sent))?;
        let uri: ureq::http::Uri = target
            .url
            .as_str()
            .parse()
            .map_err(|e: ureq::http::uri::InvalidUri| wrap_hop_err(e.to_string(), mutating_sent))?;
        return Ok((
            ureq::Agent::new_with_config(config.proxy(Some(proxy)).build()),
            uri,
        ));
    }
    let pinned = resolve_and_pin(target, policy, &SystemDnsResolver, Duration::from_secs(5))
        .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
    let connector = PinnedTcpConnector {
        addrs: pinned.addrs.clone(),
        target: target.clone(),
        policy: policy.clone(),
    }
    .chain(RustlsConnector::default());
    let resolver = PinnedResolver {
        expected_uri: pinned.uri.clone(),
        addrs: pinned.addrs,
    };
    Ok((
        ureq::Agent::with_parts(config.proxy(None).build(), connector, resolver),
        pinned.uri,
    ))
}

fn run_optional_body_request(
    agent: &ureq::Agent,
    builder: ureq::http::request::Builder,
    body_opt: Option<&str>,
    mutating_sent: bool,
) -> Result<ureq::http::Response<ureq::Body>, PluginError> {
    let is_mutation = body_opt.is_some();
    match body_opt {
        Some(body) => {
            let req = builder
                .body(body.as_bytes())
                .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
            agent
                .run(req)
                .map_err(|e| map_attendance_run_error(e, is_mutation, mutating_sent))
        }
        None => {
            let req = builder
                .body(())
                .map_err(|e| wrap_hop_err(e.to_string(), mutating_sent))?;
            agent
                .run(req)
                .map_err(|e| map_attendance_run_error(e, is_mutation, mutating_sent))
        }
    }
}

fn read_bounded_string(
    response: ureq::http::Response<ureq::Body>,
    unknown_on_err: bool,
) -> Result<String, PluginError> {
    let cap = MAX_CHECK_IN_BODY_BYTES + 1;
    let mut reader = response
        .into_body()
        .into_with_config()
        .limit(cap)
        .reader()
        .take(cap);
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| wrap_hop_err(e.to_string(), unknown_on_err))?;
    if (buf.len() as u64) > MAX_CHECK_IN_BODY_BYTES {
        return Err(wrap_hop_err(
            "response body exceeded maximum limit".into(),
            unknown_on_err,
        ));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn attendance_request(
    url: &str,
    body: Option<&str>,
    cookie: Option<&str>,
    proxy: Option<&str>,
    policy: &TargetPolicy,
) -> Result<String, PluginError> {
    let _host = validate_non_placeholder_url(url)?;
    let mut target = policy
        .normalize_url(url)
        .map_err(|e| PluginError::Fetch(hooks::sanitize_reason(&e.to_string())))?;
    let init_method = if body.is_some() { "POST" } else { "GET" };
    policy
        .validate_target_for_method(&target, Some(init_method))
        .map_err(|e| PluginError::Fetch(hooks::sanitize_reason(&e.to_string())))?;
    let initial_origin = target.origin.clone();
    let mut same_origin_chain = true;
    let mut body_opt = body;
    let mut mutating_sent = false;
    let mut cookies = Vec::new();
    let started = std::time::Instant::now();

    for hop in 0..=5 {
        let is_mutation = body_opt.is_some();
        let prior_mutating_sent = mutating_sent;
        let method = if is_mutation { "POST" } else { "GET" };
        let hop_host = target.host_ascii.clone();
        let (agent, uri) = build_guarded_hop_agent(&target, policy, proxy, mutating_sent)?;
        let mut req_builder = ureq::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "User-Agent",
                crate::http_fetcher::effective_site_user_agent(),
            );
        if is_mutation {
            req_builder = req_builder.header("Content-Type", "application/x-www-form-urlencoded");
        }
        if let Some(cookie) = cookie
            && same_origin_chain
            && target.origin == initial_origin
        {
            req_builder = req_builder.header("Cookie", cookie);
        }
        let response =
            run_optional_body_request(&agent, req_builder, body_opt, mutating_sent).map_err(
                |err| {
                    tracing::error!(site_host = %hop_host, method, %err, "Site maintenance HTTP request failed");
                    err
                },
            )?;
        if is_mutation {
            mutating_sent = true;
        }
        let status = response.status().as_u16();
        if same_origin_chain && target.origin == initial_origin {
            for value in response
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|v| v.to_str().ok())
            {
                cookies.push(format!("Set-Cookie: {value}\n"));
            }
        }
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let next_target = resolve_guarded_redirect(
                &target,
                policy,
                response.headers(),
                status,
                method,
                hop,
                mutating_sent,
            )?;
            if matches!(status, 301..=303) {
                body_opt = None;
            }
            same_origin_chain &= next_target.origin == initial_origin;
            target = next_target;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(handle_non_2xx_attendance_response(
                response,
                status,
                &hop_host,
                method,
                is_mutation,
                prior_mutating_sent,
            ));
        }
        tracing::debug!(
            site_host = %hop_host,
            method,
            elapsed_ms = started.elapsed().as_millis(),
            status,
            "Site maintenance page fetched; business outcome still requires validation"
        );
        let text = read_bounded_string(response, mutating_sent || is_mutation).map_err(|err| {
            tracing::error!(site_host = %hop_host, method, %err, "Site maintenance response read failed");
            err
        })?;
        return Ok(format!("{}{text}", cookies.concat()));
    }
    Err(wrap_hop_err("too many redirects".into(), mutating_sent))
}

pub(crate) struct StoreSites<'a> {
    state: &'a ApiState,
    observed: store::SiteObservation,
}
impl<'a> StoreSites<'a> {
    pub(crate) fn new(state: &'a ApiState, observed: store::SiteObservation) -> Self {
        Self { state, observed }
    }
}

impl SiteCredentials for StoreSites<'_> {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        if id != self.observed.site().id {
            return Err(PluginError::NotFound);
        }
        let current = self
            .state
            .store
            .observe_site(id)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
            .ok_or(PluginError::NotFound)?;
        if current != self.observed {
            return Err(PluginError::Fetch("Site changed during maintenance".into()));
        }
        Ok(current.site().clone())
    }

    fn save(&self, site: &Site) -> Result<(), PluginError> {
        if site.id != self.observed.site().id {
            return Err(PluginError::NotFound);
        }
        if crate::http::store_wait::with_foreground_store(self.state, |store| {
            store.save_site_if_current(&self.observed, site.cookie.as_deref())
        })
        .map_err(|_| PluginError::Fetch("Site maintenance write admission changed".into()))?
        .map_err(|e| PluginError::Fetch(e.to_string()))?
        {
            Ok(())
        } else {
            Err(PluginError::NotFound)
        }
    }
}

#[cfg(test)]
#[path = "check_in_tests.rs"]
mod tests;
