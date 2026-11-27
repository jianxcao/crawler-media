use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

use ureq::Agent;
use ureq::unversioned::transport::{Connector, RustlsConnector};

use crate::connector::{DeadlinePinnedTcpConnector, PinnedTcpConnector};
use crate::{
    CredentialSlot, DnsResolver, NetGuardError, NormalizedTarget, PinnedResolver, ResolvedTarget,
    SystemDnsResolver, TargetPolicy, resolve_and_pin,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardedMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

impl GuardedMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GuardedRequest {
    pub method: GuardedMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub proxy: Option<String>,
    pub max_response_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub captured_set_cookies: Vec<String>,
    pub final_url: String,
}

impl GuardedResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Clone, Debug)]
pub struct NetGuardClient {
    resolver: Arc<dyn DnsResolver>,
}

impl Default for NetGuardClient {
    fn default() -> Self {
        Self::new()
    }
}

fn is_forbidden_caller_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "host"
            | "cookie"
            | "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

fn is_stripped_response_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "set-cookie" | "cookie" | "authorization" | "proxy-authorization" | "x-api-key"
    )
}

fn validate_request_basics(
    request: &GuardedRequest,
    policy: &TargetPolicy,
) -> Result<(), NetGuardError> {
    if request
        .proxy
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty())
    {
        return Err(NetGuardError::UnsupportedRoute(
            "external proxy cannot guarantee pinned IP and SNI binding".into(),
        ));
    }
    if let Some(allowed_methods) = &policy.allowed_methods
        && !allowed_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(request.method.as_str()))
    {
        return Err(NetGuardError::MethodNotAllowed(
            request.method.as_str().into(),
        ));
    }
    if let Some(body) = &request.body
        && (body.len() as u64) > policy.limits.max_request_bytes
    {
        return Err(NetGuardError::RequestTooLarge {
            limit: policy.limits.max_request_bytes,
        });
    }
    for (name, _) in &request.headers {
        if is_forbidden_caller_header(name) {
            return Err(NetGuardError::HeaderForbidden(name.clone()));
        }
    }
    Ok(())
}

fn build_hop_agent(
    target: &NormalizedTarget,
    pinned: &ResolvedTarget,
    policy: &TargetPolicy,
    remaining: std::time::Duration,
    deadline: Instant,
) -> Agent {
    let config = ureq::config::Config::builder()
        .proxy(None)
        .max_redirects(0)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .http_status_as_error(false)
        .timeout_resolve(Some(policy.limits.resolve_timeout))
        .timeout_connect(Some(policy.limits.connect_timeout))
        .timeout_global(Some(remaining))
        .build();
    let connector = DeadlinePinnedTcpConnector {
        inner: PinnedTcpConnector {
            addrs: pinned.addrs.clone(),
            target: target.clone(),
            policy: policy.clone(),
        },
        global_deadline: Some(deadline),
    }
    .chain(RustlsConnector::default());
    let resolver = PinnedResolver {
        expected_uri: pinned.uri.clone(),
        addrs: pinned.addrs.clone(),
    };
    Agent::with_parts(config, connector, resolver)
}

fn wrap_post_send(err: NetGuardError, mutating_or_sent: bool) -> NetGuardError {
    if !mutating_or_sent {
        return err;
    }
    match err {
        NetGuardError::PostSendUnknown(_) | NetGuardError::PostSendBlocked(_) => err,
        NetGuardError::BlockedAddress(_)
        | NetGuardError::HttpsDowngradeBlocked
        | NetGuardError::MethodNotAllowed(_)
        | NetGuardError::HeaderForbidden(_)
        | NetGuardError::BodyTooLarge { .. } => NetGuardError::PostSendBlocked(Box::new(err)),
        other => NetGuardError::PostSendUnknown(other.to_string()),
    }
}

fn is_pre_send_ureq_error(err: &ureq::Error) -> bool {
    match err {
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
    }
}

fn map_ureq_error(
    err: ureq::Error,
    body_limit: u64,
    mutating: bool,
    mutating_sent: bool,
) -> NetGuardError {
    match err {
        ureq::Error::BodyExceedsLimit(_) => wrap_post_send(
            NetGuardError::BodyTooLarge { limit: body_limit },
            mutating || mutating_sent,
        ),
        ureq::Error::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::PermissionDenied => {
            wrap_post_send(
                NetGuardError::BlockedAddress(io_err.to_string()),
                mutating_sent,
            )
        }
        ureq::Error::Timeout(t @ (ureq::Timeout::Resolve | ureq::Timeout::Connect)) => {
            if mutating_sent {
                NetGuardError::PostSendUnknown(format!("timeout: {t:?}"))
            } else {
                NetGuardError::Transport(format!("timeout: {t:?}"))
            }
        }
        ureq::Error::Timeout(t) => {
            if mutating || mutating_sent {
                NetGuardError::PostSendUnknown(format!("timeout: {t:?}"))
            } else {
                NetGuardError::Timeout(format!("{t:?}"))
            }
        }
        ureq::Error::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::TimedOut => {
            if mutating || mutating_sent {
                NetGuardError::PostSendUnknown(io_err.to_string())
            } else {
                NetGuardError::Timeout(io_err.to_string())
            }
        }
        other => {
            if mutating_sent || (mutating && !is_pre_send_ureq_error(&other)) {
                NetGuardError::PostSendUnknown(other.to_string())
            } else {
                NetGuardError::Transport(other.to_string())
            }
        }
    }
}

fn read_bounded_body(
    response: ureq::http::Response<ureq::Body>,
    cap: u64,
    mutating: bool,
) -> Result<Vec<u8>, NetGuardError> {
    let read_cap = cap.saturating_add(1);
    let mut reader = response
        .into_body()
        .into_with_config()
        .limit(read_cap)
        .reader()
        .take(read_cap);
    let mut buf = Vec::new();
    match reader.read_to_end(&mut buf) {
        Ok(_) => {
            if (buf.len() as u64) > cap {
                Err(wrap_post_send(
                    NetGuardError::BodyTooLarge { limit: cap },
                    mutating,
                ))
            } else {
                Ok(buf)
            }
        }
        Err(io_err) => {
            if let Some(ureq_err) = io_err
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            {
                if matches!(ureq_err, ureq::Error::BodyExceedsLimit(_)) {
                    return Err(wrap_post_send(
                        NetGuardError::BodyTooLarge { limit: cap },
                        mutating,
                    ));
                }
                if matches!(ureq_err, ureq::Error::Timeout(_))
                    || matches!(
                        ureq_err,
                        ureq::Error::Decompress(_, inner)
                            if inner
                                .get_ref()
                                .and_then(|e| e.downcast_ref::<ureq::Error>())
                                .is_some_and(|u| matches!(u, ureq::Error::Timeout(_)))
                    )
                {
                    return Err(if mutating {
                        NetGuardError::PostSendUnknown(io_err.to_string())
                    } else {
                        NetGuardError::Timeout(io_err.to_string())
                    });
                }
            }
            if io_err.to_string().contains("BodyExceedsLimit") {
                return Err(wrap_post_send(
                    NetGuardError::BodyTooLarge { limit: cap },
                    mutating,
                ));
            }
            Err(if mutating {
                NetGuardError::PostSendUnknown(io_err.to_string())
            } else {
                NetGuardError::Transport(io_err.to_string())
            })
        }
    }
}

fn extract_headers_and_cookies(
    headers: &ureq::http::HeaderMap,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut sanitized = Vec::new();
    let mut cookies = Vec::new();
    for (name, value) in headers {
        let Ok(val_str) = value.to_str() else {
            continue;
        };
        let key = name.as_str();
        if key.eq_ignore_ascii_case("set-cookie") {
            cookies.push(val_str.to_string());
        }
        if !is_stripped_response_header(key) {
            sanitized.push((key.to_string(), val_str.to_string()));
        }
    }
    (sanitized, cookies)
}

struct HopParams<'a> {
    target: &'a NormalizedTarget,
    policy: &'a TargetPolicy,
    creds: Option<&'a CredentialSlot>,
    method: GuardedMethod,
    headers: &'a [(String, String)],
    body: Option<&'a [u8]>,
    deadline: Instant,
    response_cap: u64,
    mutating: bool,
    mutating_sent: bool,
}

struct RedirectHop<'a> {
    status: u16,
    hop: u8,
    max_redirects: u8,
    mutating: bool,
    mutating_sent: bool,
    target: &'a NormalizedTarget,
    method: GuardedMethod,
    policy: &'a TargetPolicy,
}

struct RedirectOutcome {
    next_target: NormalizedTarget,
    next_method: GuardedMethod,
    downgrade: bool,
}

fn resolve_redirect_hop(
    step: RedirectHop<'_>,
    response: ureq::http::Response<ureq::Body>,
) -> Result<RedirectOutcome, NetGuardError> {
    let mutating_or_sent = step.mutating || step.mutating_sent;
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| {
            if mutating_or_sent {
                NetGuardError::PostSendUnknown(format!(
                    "redirect {} missing Location after mutation",
                    step.status
                ))
            } else {
                NetGuardError::InvalidRedirect(format!("redirect {} missing Location", step.status))
            }
        })?;
    let _ = read_bounded_body(response, 65_536, mutating_or_sent);
    if step.hop == step.max_redirects {
        return Err(if mutating_or_sent {
            NetGuardError::PostSendUnknown(format!(
                "too many redirects after mutation (max {})",
                step.max_redirects
            ))
        } else {
            NetGuardError::TooManyRedirects(step.max_redirects)
        });
    }
    let joined = step.target.url.join(&location).map_err(|e| {
        if mutating_or_sent {
            NetGuardError::PostSendUnknown(format!("invalid redirect Location after mutation: {e}"))
        } else {
            NetGuardError::InvalidRedirect(e.to_string())
        }
    })?;
    let next_target = step
        .policy
        .normalize_url(joined.as_str())
        .map_err(|err| wrap_post_send(err, mutating_or_sent))?;
    let (next_method, downgrade) = redirect_next_method(step.status, step.method);
    if let Some(allowed_methods) = &step.policy.allowed_methods
        && !allowed_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(next_method.as_str()))
    {
        return Err(wrap_post_send(
            NetGuardError::MethodNotAllowed(next_method.as_str().into()),
            mutating_or_sent,
        ));
    }
    step.policy
        .validate_redirect_hop_for_method(step.target, &next_target, Some(next_method.as_str()))
        .map_err(|err| wrap_post_send(err, mutating_or_sent))?;
    Ok(RedirectOutcome {
        next_target,
        next_method,
        downgrade,
    })
}

fn redirect_next_method(status: u16, current: GuardedMethod) -> (GuardedMethod, bool) {
    let downgrade =
        matches!(status, 301..=303) && !matches!(current, GuardedMethod::Get | GuardedMethod::Head);
    if downgrade {
        (GuardedMethod::Get, true)
    } else {
        (current, false)
    }
}

impl NetGuardClient {
    pub fn new() -> Self {
        Self {
            resolver: Arc::new(SystemDnsResolver),
        }
    }

    pub fn with_resolver(resolver: Arc<dyn DnsResolver>) -> Self {
        Self { resolver }
    }

    fn send_hop(
        &self,
        params: HopParams<'_>,
    ) -> Result<ureq::http::Response<ureq::Body>, NetGuardError> {
        let pre_dns_remaining = params.deadline.saturating_duration_since(Instant::now());
        if pre_dns_remaining.is_zero() {
            return Err(if params.mutating_sent {
                NetGuardError::PostSendUnknown("global request timeout exceeded".into())
            } else {
                NetGuardError::Timeout("global request timeout exceeded".into())
            });
        }
        let dns_timeout = params.policy.limits.resolve_timeout.min(pre_dns_remaining);
        let pinned = resolve_and_pin(
            params.target,
            params.policy,
            self.resolver.as_ref(),
            dns_timeout,
        )
        .map_err(|err| wrap_post_send(err, params.mutating_sent))?;
        let remaining = params.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(if params.mutating_sent {
                NetGuardError::PostSendUnknown("global request timeout exceeded".into())
            } else {
                NetGuardError::Timeout("global request timeout exceeded".into())
            });
        }
        let agent = build_hop_agent(
            params.target,
            &pinned,
            params.policy,
            remaining,
            params.deadline,
        );
        let mut req_builder = ureq::http::Request::builder()
            .method(params.method.as_str())
            .uri(pinned.uri);
        for (name, value) in params.headers {
            req_builder = req_builder.header(name.as_str(), value.as_str());
        }
        if let Some(slot) = params.creds
            && let Some(auth_headers) = slot.headers_for_origin(&params.target.origin)
        {
            for (name, value) in auth_headers {
                req_builder = req_builder.header(name.as_str(), value.as_str());
            }
        }
        let result = match params.body {
            Some(bytes) => {
                let req = req_builder.body(bytes).map_err(|e| {
                    if params.mutating_sent {
                        NetGuardError::PostSendUnknown(e.to_string())
                    } else {
                        NetGuardError::InvalidUrl(e.to_string())
                    }
                })?;
                agent.run(req)
            }
            None => {
                let req = req_builder.body(()).map_err(|e| {
                    if params.mutating_sent {
                        NetGuardError::PostSendUnknown(e.to_string())
                    } else {
                        NetGuardError::InvalidUrl(e.to_string())
                    }
                })?;
                agent.run(req)
            }
        };
        result.map_err(|e| {
            map_ureq_error(
                e,
                params.response_cap,
                params.mutating,
                params.mutating_sent,
            )
        })
    }

    pub fn execute(
        &self,
        request: GuardedRequest,
        policy: &TargetPolicy,
        creds: Option<&CredentialSlot>,
    ) -> Result<GuardedResponse, NetGuardError> {
        validate_request_basics(&request, policy)?;
        let mut target = policy.normalize_url(&request.url)?;
        let initial_origin = target.origin.clone();
        policy.validate_target_for_method(&target, Some(request.method.as_str()))?;

        let max_redirects = policy.limits.max_redirects.min(5);
        let response_cap = request
            .max_response_bytes
            .map(|req_cap| req_cap.min(policy.limits.max_response_bytes))
            .unwrap_or(policy.limits.max_response_bytes);
        let deadline = Instant::now() + policy.limits.global_timeout;

        let mut method = request.method;
        let mut headers = request.headers;
        let mut body = request.body;
        let mut captured_cookies = Vec::new();

        let mut mutating_sent = false;
        let mut same_origin_chain = true;

        for hop in 0..=max_redirects {
            let mutating = !matches!(method, GuardedMethod::Get | GuardedMethod::Head);
            let response = self.send_hop(HopParams {
                target: &target,
                policy,
                creds: creds.filter(|_| same_origin_chain),
                method,
                headers: &headers,
                body: body.as_deref(),
                deadline,
                response_cap,
                mutating,
                mutating_sent,
            })?;
            if mutating {
                mutating_sent = true;
            }
            let status = response.status().as_u16();
            let (sanitized_headers, mut hop_cookies) =
                extract_headers_and_cookies(response.headers());
            if same_origin_chain
                && target.origin == initial_origin
                && creds.is_none_or(|slot| slot.bound_origin == target.origin)
            {
                captured_cookies.append(&mut hop_cookies);
            }

            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let outcome = resolve_redirect_hop(
                    RedirectHop {
                        status,
                        hop,
                        max_redirects,
                        mutating,
                        mutating_sent,
                        target: &target,
                        method,
                        policy,
                    },
                    response,
                )?;
                if outcome.downgrade {
                    method = outcome.next_method;
                    body = None;
                    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
                }
                tracing::debug!(
                    hop = hop + 1,
                    from = %target.origin,
                    to = %outcome.next_target.origin,
                    status,
                    "net-guard following validated redirect"
                );
                same_origin_chain &= outcome.next_target.origin == initial_origin;
                target = outcome.next_target;
                continue;
            }

            let resp_body = read_bounded_body(response, response_cap, mutating || mutating_sent)?;
            return Ok(GuardedResponse {
                status,
                headers: sanitized_headers,
                body: resp_body,
                captured_set_cookies: captured_cookies,
                final_url: target.url.to_string(),
            });
        }

        Err(if mutating_sent {
            NetGuardError::PostSendUnknown(format!(
                "too many redirects after mutation (max {max_redirects})"
            ))
        } else {
            NetGuardError::TooManyRedirects(max_redirects)
        })
    }
}
