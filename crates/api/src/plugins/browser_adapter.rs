#[path = "browser_session.rs"]
mod session;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use domain::{MaintenanceVia, SiteId};
use indexer::{PluginBrowserError, PluginBrowserErrorKind};
use plugin_api::{
    ApiError, BrowserPurpose, ErrorCode, ErrorDetail, GuestRole, PluginTrust, UpstreamKind,
};
use plugin_host::{BrowserSessionPort, HostPorts, Invocation};

use self::session::ObscuraBrowserSession;
use super::ports::{ActiveAuth, ActiveInvocation, StoreHostPorts, api_err, map_store_error};
use super::wasm_runner::resolve_plugin_trust;

const MAX_BROWSER_BYTES: usize = 5 * 1024 * 1024;
const SENSITIVE_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
];

fn check_browser_trust_and_grant(
    ports: &StoreHostPorts,
    ctx: &ActiveInvocation,
    inv: &Invocation,
    purpose: BrowserPurpose,
) -> Result<(), ApiError> {
    let sig_status = ctx
        .deps
        .store
        .get_plugin(&inv.plugin_id)
        .map_err(map_store_error)?
        .and_then(|p| {
            ctx.deps
                .store
                .get_plugin_version(&p.plugin_id, &p.package_hash)
                .ok()
                .flatten()
        })
        .map(|v| v.signature_status)
        .unwrap_or_default();
    let trust = resolve_plugin_trust(&inv.plugin_id, &sig_status, &ctx.deps.admin_trusted_plugins);
    if !matches!(trust, PluginTrust::BuiltIn | PluginTrust::AdminTrusted) {
        return Err(browser_unavailable(
            "browser session requires built-in or admin-trusted plugin",
        ));
    }
    let cap = match purpose {
        BrowserPurpose::Search => "browser:search",
        BrowserPurpose::Maintenance => "browser:maintenance",
    };
    if !ports.authorized(inv, cap) {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            &format!("missing {cap} grant"),
        ));
    }
    Ok(())
}

fn load_gated_browser_site(
    ctx: &ActiveInvocation,
    inv: &Invocation,
    purpose: BrowserPurpose,
) -> Result<
    (
        SiteId,
        store::SiteObservation,
        String,
        store::plugins::PluginStateAuthority,
    ),
    ApiError,
> {
    let ActiveAuth::Runtime {
        authority,
        invocation_site,
        ..
    } = &ctx.auth
    else {
        return Err(api_err(
            ErrorCode::WrongContext,
            "browser port unavailable in lifecycle mode",
        ));
    };
    ctx.deps
        .store
        .validate_plugin_state_authority(authority)
        .map_err(map_store_error)?;
    let site_id = authority
        .site_id()
        .or_else(|| invocation_site.as_ref().map(|(id, _)| *id))
        .ok_or_else(|| {
            api_err(
                ErrorCode::WrongContext,
                "browser session requires bound site_id",
            )
        })?;
    let obs = ctx
        .deps
        .store
        .observe_site(site_id)
        .map_err(map_store_error)?
        .ok_or_else(|| api_err(ErrorCode::NotFound, "bound site not found"))?;
    if !obs.site().enabled {
        return Err(api_err(
            ErrorCode::StaleGeneration,
            "bound site is disabled",
        ));
    }
    let current_epoch = obs.credential_epoch().storage_key().to_string();
    if let Some(inv_ep) = inv.credential_epoch.as_deref()
        && inv_ep != current_epoch
    {
        return Err(api_err(
            ErrorCode::StaleGeneration,
            "site credential epoch changed",
        ));
    }
    if obs
        .site()
        .proxy
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty())
    {
        return Err(ApiError {
            code: ErrorCode::Unsupported,
            message: "browser session cannot run on site with proxy".into(),
            detail: Some(ErrorDetail::UnsupportedRoute),
            retry_after_ms: None,
            operation_id: None,
        });
    }
    if !ctx.deps.browser.cdp_enabled().unwrap_or(false) {
        return Err(browser_unavailable("global CDP browser is disabled"));
    }
    let site_gate_ok = match purpose {
        BrowserPurpose::Search => obs.site().browser_search,
        BrowserPurpose::Maintenance => obs.site().maintenance_via == MaintenanceVia::Browser,
    };
    if !site_gate_ok {
        return Err(browser_unavailable(
            "site browser policy disabled for purpose",
        ));
    }
    Ok((site_id, obs, current_epoch, authority.clone()))
}

pub(super) fn open_browser_session(
    ports: &StoreHostPorts,
    inv: &Invocation,
    purpose: BrowserPurpose,
) -> Result<Box<dyn BrowserSessionPort>, ApiError> {
    if inv.role == GuestRole::Lifecycle {
        return Err(api_err(
            ErrorCode::WrongContext,
            "browser port is forbidden during lifecycle validation",
        ));
    }
    if inv.cancelled() {
        return Err(api_err(ErrorCode::Cancelled, "plugin invocation cancelled"));
    }
    if inv.expired() {
        return Err(api_err(
            ErrorCode::DeadlineExceeded,
            "plugin invocation deadline expired",
        ));
    }
    let role_ok = matches!(
        (inv.role, purpose),
        (GuestRole::Indexer, BrowserPurpose::Search)
            | (GuestRole::Maintenance, BrowserPurpose::Maintenance)
    );
    if !role_ok {
        return Err(api_err(
            ErrorCode::WrongContext,
            "browser purpose does not match invocation role",
        ));
    }

    let ctx = ports.lookup(inv)?;
    let ctx_role_ok = matches!(
        (ctx.role, purpose),
        (
            super::effect::PluginInvocationRole::Indexer,
            BrowserPurpose::Search
        ) | (
            super::effect::PluginInvocationRole::Maintenance,
            BrowserPurpose::Maintenance
        )
    );
    if !ctx_role_ok {
        return Err(api_err(
            ErrorCode::WrongContext,
            "browser purpose does not match invocation role",
        ));
    }
    check_browser_trust_and_grant(ports, &ctx, inv, purpose)?;
    let (site_id, obs, current_epoch, authority) = load_gated_browser_site(&ctx, inv, purpose)?;

    let site_norm = validate_browser_site_target(&ctx, &obs)?;
    let mut lease = ctx
        .deps
        .browser
        .open_plugin_session(&obs.site().url, obs.site().cdp_url.as_deref(), inv.deadline)
        .map_err(map_browser_err)?;

    if let Some(cookie) = obs
        .site()
        .cookie
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        && let Err(err) = lease.set_cookies(&obs.site().url, cookie)
    {
        let _ = lease.close_verified();
        return Err(map_browser_err(err));
    }

    Ok(Box::new(ObscuraBrowserSession {
        ports: Arc::new(ports.clone_handle()),
        store: ctx.deps.store.clone(),
        authority,
        site_id,
        site_url: obs.site().url.clone(),
        site_origin: site_norm.origin,
        credential_epoch: current_epoch,
        purpose,
        deadline: inv.deadline,
        lease,
        next_seq: 1,
        element_handles: HashMap::new(),
        nav_responses: HashMap::new(),
        post_nav_or_click: false,
        has_navigated: false,
    }))
}

fn blocked_browser_addr(msg: impl Into<String>) -> ApiError {
    ApiError {
        code: ErrorCode::PermissionDenied,
        message: msg.into(),
        detail: Some(ErrorDetail::BlockedAddress),
        retry_after_ms: None,
        operation_id: None,
    }
}

fn normalize_cdp_endpoint(raw: &str) -> Option<net_guard::NormalizedTarget> {
    let trimmed = raw.trim();
    let http_equiv = if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{rest}")
    } else {
        trimmed.to_string()
    };
    net_guard::normalize_url(&http_equiv, true).ok()
}

fn validate_browser_site_target(
    ctx: &ActiveInvocation,
    obs: &store::SiteObservation,
) -> Result<net_guard::NormalizedTarget, ApiError> {
    let site_url = &obs.site().url;
    let site_norm = net_guard::normalize_url(site_url, true).map_err(|e| match e {
        net_guard::NetGuardError::BlockedAddress(msg) => blocked_browser_addr(msg),
        other => browser_unavailable(&other.to_string()),
    })?;
    let host_lc = site_norm
        .origin
        .host
        .trim_end_matches('.')
        .to_ascii_lowercase();

    let mut policy = ctx
        .deps
        .plugin_http_policy_override
        .clone()
        .unwrap_or_else(net_guard::TargetPolicy::production);
    if let Some(self_addr) = ctx.deps.self_listen {
        policy = policy.with_blocked_self(self_addr);
    }
    if let Ok(cfg) = ctx.deps.browser.effective_config()
        && let Ok(Some((cdp_raw, _))) = cfg.resolve(obs.site().cdp_url.as_deref())
        && let Some(cdp_norm) = normalize_cdp_endpoint(&cdp_raw)
    {
        let cdp_host_lc = cdp_norm
            .origin
            .host
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if cdp_norm.port == site_norm.port && cdp_host_lc == host_lc {
            return Err(blocked_browser_addr("browser target matches CDP endpoint"));
        }
        if let Some(cdp_ip) = cdp_norm.host_is_ip {
            policy = policy.with_blocked_self(SocketAddr::new(cdp_ip, cdp_norm.port));
            if cdp_ip.is_loopback() {
                policy = policy.with_blocked_self(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    cdp_norm.port,
                ));
                policy = policy.with_blocked_self(SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::LOCALHOST),
                    cdp_norm.port,
                ));
            }
        } else if cdp_host_lc == "localhost" || cdp_host_lc.ends_with(".localhost") {
            policy = policy.with_blocked_self(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                cdp_norm.port,
            ));
            policy = policy.with_blocked_self(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::LOCALHOST),
                cdp_norm.port,
            ));
        }
    }

    policy.validate_target(&site_norm).map_err(|e| match e {
        net_guard::NetGuardError::BlockedAddress(msg) => blocked_browser_addr(msg),
        other => blocked_browser_addr(other.to_string()),
    })?;
    if let Some(ip) = site_norm.host_is_ip {
        policy
            .validate_socket_addr(&site_norm, SocketAddr::new(ip, site_norm.port))
            .map_err(|e| match e {
                net_guard::NetGuardError::BlockedAddress(msg) => blocked_browser_addr(msg),
                other => blocked_browser_addr(other.to_string()),
            })?;
    }

    if site_norm.host_is_ip.is_none() {
        let is_localhost = host_lc == "localhost" || host_lc.ends_with(".localhost");
        let is_non_public_name = host_lc.ends_with(".local")
            || host_lc.ends_with(".internal")
            || (!is_localhost && !host_lc.contains('.'));
        if is_non_public_name || (is_localhost && ctx.deps.plugin_http_policy_override.is_none()) {
            return Err(blocked_browser_addr(format!(
                "private hostname `{}:{}` is blocked",
                site_norm.origin.host, site_norm.port
            )));
        }
        if is_localhost {
            for ip in [
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ] {
                let sock = SocketAddr::new(ip, site_norm.port);
                if policy.is_self_blocked(sock) {
                    return Err(blocked_browser_addr(format!(
                        "self or CDP address `{sock}` is blocked"
                    )));
                }
            }
            let v4_ok = policy
                .validate_socket_addr(
                    &site_norm,
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), site_norm.port),
                )
                .is_ok();
            let v6_ok = policy
                .validate_socket_addr(
                    &site_norm,
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), site_norm.port),
                )
                .is_ok();
            if !v4_ok && !v6_ok {
                return Err(blocked_browser_addr(format!(
                    "loopback hostname `{}:{}` is blocked",
                    site_norm.origin.host, site_norm.port
                )));
            }
        }
    }
    Ok(site_norm)
}

fn browser_unavailable(msg: &str) -> ApiError {
    ApiError {
        code: ErrorCode::Unsupported,
        message: msg.into(),
        detail: Some(ErrorDetail::BrowserIsolationUnavailable),
        retry_after_ms: None,
        operation_id: None,
    }
}

fn map_browser_err(err: PluginBrowserError) -> ApiError {
    match err.kind {
        PluginBrowserErrorKind::Busy => api_err(ErrorCode::Busy, &err.message),
        PluginBrowserErrorKind::Incompatible | PluginBrowserErrorKind::Setup => {
            browser_unavailable(&err.message)
        }
        PluginBrowserErrorKind::InvalidArgument => {
            api_err(ErrorCode::InvalidArgument, &err.message)
        }
        PluginBrowserErrorKind::NotFound => api_err(ErrorCode::NotFound, &err.message),
        PluginBrowserErrorKind::PostNavUnknown => api_err(ErrorCode::UnknownOutcome, &err.message),
        PluginBrowserErrorKind::Transport => ApiError {
            code: ErrorCode::Upstream,
            message: err.message,
            detail: Some(ErrorDetail::Upstream(UpstreamKind::Network)),
            retry_after_ms: None,
            operation_id: None,
        },
    }
}
