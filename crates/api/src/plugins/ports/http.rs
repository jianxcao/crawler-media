use std::net::IpAddr;

use domain::SiteId;
use net_guard::{
    CredentialSlot, GuardedMethod, GuardedRequest, NetGuardError, Origin, TargetPolicy,
    canonicalize_ip, is_cloud_metadata_ip, normalize_url, path_matches_prefix,
};
use plugin_api::{
    ApiError, ErrorCode, ErrorDetail, GuestRole, Header, HttpRequest, HttpResponse, HttpTarget,
    UpstreamKind,
};
use plugin_host::HostPorts;
use std::fmt::Write as _;

use super::{
    ActiveAuth, ActiveInvocation, CapturedCookieRecord, StoreHostPorts, api_err, map_store_error,
};

const FORBIDDEN_CALLER_HEADERS: &[&str] = &[
    "host",
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "connection",
    "content-length",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const SENSITIVE_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
];

type PreparedGuardedHttp = (
    GuardedRequest,
    TargetPolicy,
    Option<CredentialSlot>,
    Option<(SiteId, String, Origin)>,
);

fn prepare_http_invocation(
    ports: &StoreHostPorts,
    inv: &InvocationProxy<'_>,
    req: HttpRequest,
) -> Result<PreparedGuardedHttp, ApiError> {
    if inv.inv.role == GuestRole::Lifecycle {
        return Err(api_err(
            ErrorCode::WrongContext,
            "http is forbidden during lifecycle validation",
        ));
    }
    if inv.inv.cancelled() {
        return Err(api_err(ErrorCode::Cancelled, "plugin invocation cancelled"));
    }
    if inv.inv.expired() {
        return Err(api_err(
            ErrorCode::DeadlineExceeded,
            "plugin invocation deadline expired",
        ));
    }

    let ctx = ports.lookup(inv.inv)?;
    if ctx.role == crate::plugins::PluginInvocationRole::Lifecycle {
        return Err(api_err(
            ErrorCode::WrongContext,
            "http is forbidden during lifecycle validation",
        ));
    }
    if !ports.authorized(inv.inv, "http") {
        return Err(api_err(ErrorCode::PermissionDenied, "missing http grant"));
    }
    let method = parse_method(&req.method)?;
    if (matches!(inv.inv.role, GuestRole::Filter | GuestRole::Gate)
        || matches!(
            ctx.role,
            crate::plugins::PluginInvocationRole::Filter
                | crate::plugins::PluginInvocationRole::Gate
                | crate::plugins::PluginInvocationRole::ReadOnlyEndpoint
        ))
        && method != GuardedMethod::Get
    {
        return Err(api_err(
            ErrorCode::WrongContext,
            "read-only plugin contexts only allow GET requests",
        ));
    }
    if req.max_bytes == 0 {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "http max_bytes must be > 0",
        ));
    }
    let headers = validate_caller_headers(&req.headers)?;

    let PreparedHttp {
        url,
        policy,
        creds,
        capture_meta,
    } = match &req.target {
        HttpTarget::PublicUrl(raw) => prepare_public_url(&ctx, method, raw)?,
        HttpTarget::SiteRelative(target) => prepare_site_relative(ports, inv.inv, &ctx, target)?,
    };

    let response_cap = ctx
        .max_response_bytes
        .map_or(req.max_bytes, |limit| req.max_bytes.min(limit));
    let guarded_req = GuardedRequest {
        method,
        url,
        headers,
        body: req.body,
        proxy: None,
        max_response_bytes: Some(response_cap),
    };
    Ok((guarded_req, policy, creds, capture_meta))
}

pub(super) fn http_fetch(
    ports: &StoreHostPorts,
    inv: &InvocationProxy<'_>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let (guarded_req, policy, creds, capture_meta) = prepare_http_invocation(ports, inv, req)?;

    let resp = ports
        .http_client
        .execute(guarded_req, &policy, creds.as_ref())
        .map_err(map_net_guard_error)?;

    if inv.inv.cancelled() {
        return Err(api_err(ErrorCode::Cancelled, "plugin invocation cancelled"));
    }
    if inv.inv.expired() {
        return Err(api_err(
            ErrorCode::DeadlineExceeded,
            "plugin invocation deadline expired",
        ));
    }

    let receipt = if let Some((site_id, epoch, origin)) = capture_meta
        && !resp.captured_set_cookies.is_empty()
    {
        Some(ports.record_captured_cookies(CapturedCookieRecord {
            site_id,
            credential_epoch: epoch,
            origin,
            set_cookies: resp.captured_set_cookies,
            merged_cookie: None,
        }))
    } else {
        None
    };

    let clean_headers = resp
        .headers
        .into_iter()
        .filter(|(k, _)| {
            let lower = k.trim().to_ascii_lowercase();
            !SENSITIVE_RESPONSE_HEADERS.contains(&lower.as_str())
        })
        .map(|(name, value)| Header { name, value })
        .collect();

    Ok(HttpResponse {
        status: resp.status,
        headers: clean_headers,
        body: resp.body,
        truncated: false,
        receipt,
    })
}

pub(super) fn http_fetch_many(
    ports: &StoreHostPorts,
    inv: &plugin_host::Invocation,
    reqs: Vec<HttpRequest>,
) -> Vec<Result<HttpResponse, ApiError>> {
    if reqs.is_empty() {
        return vec![Err(api_err(
            ErrorCode::InvalidArgument,
            "fetch-many requires 1..=8 requests",
        ))];
    }
    if reqs.len() > 8 {
        return vec![
            Err(api_err(
                ErrorCode::InvalidArgument,
                "fetch-many allows at most 8 requests",
            ));
            reqs.len()
        ];
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = reqs
            .into_iter()
            .map(|req| scope.spawn(move || http_fetch(ports, &InvocationProxy { inv }, req)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join().unwrap_or_else(|_| {
                    Err(api_err(
                        ErrorCode::UnknownOutcome,
                        "bounded http worker panicked",
                    ))
                })
            })
            .collect()
    })
}

pub(super) struct InvocationProxy<'a> {
    pub(super) inv: &'a plugin_host::Invocation,
}

struct PreparedHttp {
    url: String,
    policy: TargetPolicy,
    creds: Option<CredentialSlot>,
    capture_meta: Option<(SiteId, String, Origin)>,
}

fn parse_method(raw: &str) -> Result<GuardedMethod, ApiError> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "GET" => Ok(GuardedMethod::Get),
        "POST" => Ok(GuardedMethod::Post),
        "PUT" => Ok(GuardedMethod::Put),
        "DELETE" => Ok(GuardedMethod::Delete),
        "PATCH" => Ok(GuardedMethod::Patch),
        "HEAD" => Ok(GuardedMethod::Head),
        _ => Err(api_err(
            ErrorCode::InvalidArgument,
            "unsupported http method",
        )),
    }
}

fn validate_caller_headers(headers: &[Header]) -> Result<Vec<(String, String)>, ApiError> {
    let mut out = Vec::with_capacity(headers.len());
    for h in headers {
        let lower = h.name.trim().to_ascii_lowercase();
        if lower.is_empty()
            || h.name.chars().any(|c| c.is_control() || c.is_whitespace())
            || h.value.chars().any(char::is_control)
            || FORBIDDEN_CALLER_HEADERS.contains(&lower.as_str())
        {
            return Err(api_err(
                ErrorCode::PermissionDenied,
                "forbidden request header",
            ));
        }
        out.push((h.name.clone(), h.value.clone()));
    }
    Ok(out)
}

fn base_policy(ctx: &ActiveInvocation) -> TargetPolicy {
    let mut policy = ctx
        .deps
        .plugin_http_policy_override
        .clone()
        .unwrap_or_else(TargetPolicy::production);
    if let Some(self_addr) = ctx.deps.self_listen {
        policy = policy.with_blocked_self(self_addr);
    }
    policy
}

fn prepare_public_url(
    ctx: &ActiveInvocation,
    method: GuardedMethod,
    raw_url: &str,
) -> Result<PreparedHttp, ApiError> {
    if let ActiveAuth::Runtime { authority, .. } = &ctx.auth {
        ctx.deps
            .store
            .validate_plugin_state_authority(authority)
            .map_err(map_store_error)?;
    }
    let policy = attach_manifest_http_policies(ctx, base_policy(ctx));
    let target = resolve_public_target(ctx, &policy, method, raw_url)?;

    if let Some(ip) = target.host_is_ip {
        let sock = std::net::SocketAddr::new(ip, target.port);
        if policy.is_self_blocked(sock) || canonicalize_ip(ip).is_unspecified() {
            return Err(map_net_guard_error(NetGuardError::BlockedAddress(format!(
                "self-api or unspecified address `{sock}` is blocked"
            ))));
        }
    }

    Ok(PreparedHttp {
        url: target.url.to_string(),
        policy,
        creds: None,
        capture_meta: None,
    })
}

fn attach_manifest_http_policies(ctx: &ActiveInvocation, mut policy: TargetPolicy) -> TargetPolicy {
    for dest in &*ctx.manifest_http {
        let Ok(dest_norm) = normalize_url(&dest.origin, true) else {
            continue;
        };
        policy = policy.with_allowed_origin_methods(
            dest_norm.origin.clone(),
            vec![dest.path_prefix.clone()],
            dest.methods.clone(),
        );
        if ctx.deps.plugin_http_policy_override.is_none()
            && let Some(ex) = crate::http_fetcher::private_endpoint_exception_for_norm(
                &dest_norm,
                &policy,
                vec![dest.path_prefix.clone()],
            )
        {
            policy = policy.with_private_exception(ex);
        }
    }
    policy
}

fn resolve_public_target(
    ctx: &ActiveInvocation,
    policy: &TargetPolicy,
    method: GuardedMethod,
    raw_url: &str,
) -> Result<net_guard::NormalizedTarget, ApiError> {
    if raw_url.starts_with('/') {
        if raw_url.starts_with("//") {
            return Err(api_err(
                ErrorCode::InvalidArgument,
                "relative http target must not start with '//'",
            ));
        }
        let _ = normalize_url(&format!("http://placeholder.invalid{raw_url}"), true)
            .map_err(map_net_guard_error)?;

        let mut best: Option<(usize, net_guard::NormalizedTarget)> = None;
        for dest in &*ctx.manifest_http {
            if !dest
                .methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(method.as_str()))
            {
                continue;
            }
            let Ok(dest_norm) = normalize_url(&dest.origin, true) else {
                continue;
            };
            let candidate = format!("{}{}", dest_norm.origin, raw_url);
            let Ok(cand_target) = policy.normalize_url(&candidate) else {
                continue;
            };
            if cand_target.origin == dest_norm.origin
                && path_matches_prefix(cand_target.url.path(), &dest.path_prefix)
                && best
                    .as_ref()
                    .is_none_or(|(len, _)| dest.path_prefix.len() > *len)
            {
                best = Some((dest.path_prefix.len(), cand_target));
            }
        }
        return best.map(|(_, t)| t).ok_or_else(|| {
            api_err(
                ErrorCode::PermissionDenied,
                "destination url or method is not declared in plugin manifest.http",
            )
        });
    }

    let target = policy.normalize_url(raw_url).map_err(map_net_guard_error)?;
    let is_declared = ctx.manifest_http.iter().any(|dest| {
        normalize_url(&dest.origin, true).is_ok_and(|o| o.origin == target.origin)
            && path_matches_prefix(target.url.path(), &dest.path_prefix)
            && dest
                .methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(method.as_str()))
    });
    if !is_declared {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "destination url or method is not declared in plugin manifest.http",
        ));
    }
    Ok(target)
}

fn validate_site_relative_path(path: &str) -> Result<(), ApiError> {
    if path.chars().any(|c| c.is_control() || c.is_whitespace())
        || path.contains('\\')
        || path.contains('?')
        || path.contains('#')
        || path.starts_with("//")
        || path.split('/').any(|seg| seg == "..")
    {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "invalid site-relative path",
        ));
    }
    let lower = path.to_ascii_lowercase();
    let prefix = lower.split(['/', '?', '#']).next().unwrap_or("");
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.contains("://")
        || prefix.contains(':')
    {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "site-relative path cannot specify scheme",
        ));
    }
    Ok(())
}

fn percent_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            let _ = write!(&mut out, "%{byte:02X}");
        }
    }
    out
}

fn build_site_auth_slot(
    ports: &StoreHostPorts,
    inv: &plugin_host::Invocation,
    obs: &store::SiteObservation,
    site_id: SiteId,
    site_origin: &Origin,
    current_epoch: &str,
    auth_slot: Option<&str>,
) -> Result<Option<CredentialSlot>, ApiError> {
    let Some(slot) = auth_slot else {
        return Ok(None);
    };
    if !ports.authorized(inv, "http:site-auth") {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "missing http:site-auth grant",
        ));
    }
    if inv
        .credential_epoch
        .as_deref()
        .is_none_or(|ep| ep.trim().is_empty())
    {
        return Err(api_err(
            ErrorCode::WrongContext,
            "http:site-auth requires bound credential_epoch",
        ));
    }
    let stripped = super::site::strip_site_prefix(slot);
    if stripped.is_empty() {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "auth_slot must not be empty",
        ));
    }
    if slot
        .trim()
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("site:"))
        && stripped != site_id.to_string()
    {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "auth_slot cannot reference another site",
        ));
    }
    let mut auth_headers = Vec::new();
    if let Some(cookie) = obs
        .site()
        .cookie
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        auth_headers.push(("Cookie".into(), cookie.to_string()));
    }
    if let Some(api_key) = obs
        .site()
        .api_key
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        auth_headers.push(("X-Api-Key".into(), api_key.to_string()));
    }
    Ok(Some(CredentialSlot::new(
        site_id.to_string(),
        site_origin.clone(),
        current_epoch.to_string(),
        auth_headers,
    )))
}

fn build_site_target_url(
    site_norm: &net_guard::NormalizedTarget,
    site_origin: &Origin,
    target: &plugin_api::SiteTarget,
) -> String {
    let base_path = site_norm.url.path().trim_end_matches('/');
    let rel_path = target.path.trim_start_matches('/');
    let combined_path = if rel_path.is_empty() {
        if base_path.is_empty() {
            "/".to_string()
        } else {
            base_path.to_string()
        }
    } else {
        format!("{base_path}/{rel_path}")
    };
    let mut url = format!("{site_origin}{combined_path}");
    if !target.query.is_empty() {
        let query = target
            .query
            .iter()
            .map(|h| {
                format!(
                    "{}={}",
                    percent_encode_component(&h.name),
                    percent_encode_component(&h.value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&query);
    }
    url
}

fn prepare_site_relative(
    ports: &StoreHostPorts,
    inv: &plugin_host::Invocation,
    ctx: &ActiveInvocation,
    target: &plugin_api::SiteTarget,
) -> Result<PreparedHttp, ApiError> {
    validate_site_relative_path(&target.path)?;
    let ActiveAuth::Runtime {
        authority,
        invocation_site,
        ..
    } = &ctx.auth
    else {
        return Err(api_err(
            ErrorCode::WrongContext,
            "site-relative http unavailable in lifecycle mode",
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
                "site-relative http target requires bound site_id",
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

    if let Some(inv_epoch) = inv.credential_epoch.as_deref()
        && inv_epoch != current_epoch
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
            message: "site external proxy cannot guarantee pinned IP/SNI routing".into(),
            detail: Some(ErrorDetail::UnsupportedRoute),
            retry_after_ms: None,
            operation_id: None,
        });
    }

    let site_norm = normalize_url(&obs.site().url, true).map_err(map_net_guard_error)?;
    let site_origin = site_norm.origin.clone();
    let creds = build_site_auth_slot(
        ports,
        inv,
        &obs,
        site_id,
        &site_origin,
        &current_epoch,
        target.auth_slot.as_deref(),
    )?;
    let url = build_site_target_url(&site_norm, &site_origin, target);

    let mut policy = base_policy(ctx).with_allowed_origin(site_origin.clone(), vec!["/".into()]);
    if let Some(ip) = site_norm.host_is_ip
        && !is_cloud_metadata_ip(ip)
        && ctx.deps.plugin_http_policy_override.is_none()
        && is_non_global_ip(ip)
    {
        let sock = std::net::SocketAddr::new(ip, site_norm.port);
        if policy.is_self_blocked(sock) || canonicalize_ip(ip).is_unspecified() {
            return Err(map_net_guard_error(NetGuardError::BlockedAddress(format!(
                "self-api or unspecified address `{sock}` is blocked"
            ))));
        }
        if let Some(ex) = crate::http_fetcher::private_endpoint_exception_for_norm(
            &site_norm,
            &policy,
            vec!["/".into()],
        ) {
            policy = policy.with_private_exception(ex);
        }
    }

    let capture_meta = (inv.role == GuestRole::Maintenance
        && ctx.role == crate::plugins::effect::PluginInvocationRole::Maintenance
        && ports.authorized(inv, "http:site-auth")
        && inv
            .credential_epoch
            .as_deref()
            .is_some_and(|ep| !ep.trim().is_empty()))
    .then_some((site_id, current_epoch, site_origin));

    Ok(PreparedHttp {
        url,
        policy,
        creds,
        capture_meta,
    })
}

fn is_non_global_ip(ip: IpAddr) -> bool {
    !net_guard::is_global_unicast(ip)
}

fn map_net_guard_error(err: NetGuardError) -> ApiError {
    match err {
        NetGuardError::PostSendBlocked(inner) => {
            let mut mapped = map_net_guard_error(*inner);
            mapped.code = ErrorCode::UnknownOutcome;
            mapped
        }
        NetGuardError::BlockedAddress(msg) => ApiError {
            code: ErrorCode::PermissionDenied,
            message: msg,
            detail: Some(ErrorDetail::BlockedAddress),
            retry_after_ms: None,
            operation_id: None,
        },
        NetGuardError::UnsupportedRoute(msg) => ApiError {
            code: ErrorCode::Unsupported,
            message: msg,
            detail: Some(ErrorDetail::UnsupportedRoute),
            retry_after_ms: None,
            operation_id: None,
        },
        NetGuardError::BodyTooLarge { limit } => ApiError {
            code: ErrorCode::QuotaExceeded,
            message: format!("response body exceeds limit of {limit} bytes"),
            detail: Some(ErrorDetail::BodyTooLarge),
            retry_after_ms: None,
            operation_id: None,
        },
        NetGuardError::MethodNotAllowed(msg) | NetGuardError::HeaderForbidden(msg) => {
            api_err(ErrorCode::PermissionDenied, &msg)
        }
        NetGuardError::RequestTooLarge { .. } | NetGuardError::InvalidUrl(_) => {
            api_err(ErrorCode::InvalidArgument, &err.to_string())
        }
        NetGuardError::Timeout(msg) => api_err(ErrorCode::DeadlineExceeded, &msg),
        NetGuardError::PostSendUnknown(msg) => api_err(ErrorCode::UnknownOutcome, &msg),
        NetGuardError::HttpsDowngradeBlocked
        | NetGuardError::TooManyRedirects(_)
        | NetGuardError::InvalidRedirect(_)
        | NetGuardError::Transport(_) => ApiError {
            code: ErrorCode::Upstream,
            message: err.to_string(),
            detail: Some(ErrorDetail::Upstream(UpstreamKind::Network)),
            retry_after_ms: None,
            operation_id: None,
        },
    }
}
