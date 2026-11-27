use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::ip::{is_cloud_metadata_host, is_cloud_metadata_ip, is_global_unicast};
use crate::{NetGuardError, NormalizedTarget, Origin};

pub const DEFAULT_MAX_REQUEST_BYTES: u64 = 1_048_576;
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 5_242_880;
pub const TORRENT_MAX_RESPONSE_BYTES: u64 = 10_485_760;
pub const MAX_ALLOWED_REDIRECTS: u8 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetGuardLimits {
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub max_redirects: u8,
    pub resolve_timeout: Duration,
    pub connect_timeout: Duration,
    pub global_timeout: Duration,
}

impl Default for NetGuardLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_redirects: MAX_ALLOWED_REDIRECTS,
            resolve_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(8),
            global_timeout: Duration::from_secs(20),
        }
    }
}

impl NetGuardLimits {
    pub fn torrent() -> Self {
        Self {
            max_response_bytes: TORRENT_MAX_RESPONSE_BYTES,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateEndpointException {
    pub host: String,
    pub port: u16,
    pub allowed_ips: Vec<IpAddr>,
    pub path_prefixes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginScope {
    pub origin: Origin,
    pub path_prefixes: Vec<String>,
    pub methods: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct TargetPolicy {
    pub limits: NetGuardLimits,
    pub allowed_methods: Option<Vec<String>>,
    pub allowed_origins: Option<Vec<OriginScope>>,
    pub private_exceptions: Vec<PrivateEndpointException>,
    pub blocked_self: Vec<SocketAddr>,
    test_loopback_sockets: Vec<SocketAddr>,
}

pub fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    if prefix.ends_with('/') {
        return path.starts_with(prefix);
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl TargetPolicy {
    pub fn production() -> Self {
        Self::default()
    }

    pub fn with_limits(mut self, limits: NetGuardLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_blocked_self(mut self, addr: SocketAddr) -> Self {
        self.blocked_self.push(addr);
        self
    }

    pub fn with_private_exception(mut self, exception: PrivateEndpointException) -> Self {
        self.private_exceptions.push(exception);
        self
    }

    pub fn with_allowed_origin(mut self, origin: Origin, path_prefixes: Vec<String>) -> Self {
        self.allowed_origins
            .get_or_insert_with(Vec::new)
            .push(OriginScope {
                origin,
                path_prefixes,
                methods: None,
            });
        self
    }

    pub fn with_allowed_origin_methods(
        mut self,
        origin: Origin,
        path_prefixes: Vec<String>,
        methods: Vec<String>,
    ) -> Self {
        self.allowed_origins
            .get_or_insert_with(Vec::new)
            .push(OriginScope {
                origin,
                path_prefixes,
                methods: Some(methods),
            });
        self
    }

    pub fn with_allowed_methods(mut self, methods: Vec<String>) -> Self {
        self.allowed_methods = Some(methods);
        self
    }

    pub fn with_test_loopback_socket(mut self, addr: SocketAddr) -> Self {
        self.test_loopback_sockets.push(addr);
        self
    }

    pub fn allows_private_hostname_for(&self, host: &str, port: u16) -> bool {
        !self.test_loopback_sockets.is_empty()
            || self
                .private_exceptions
                .iter()
                .any(|ex| ex.host.eq_ignore_ascii_case(host) && ex.port == port)
    }

    pub fn normalize_url(&self, raw: &str) -> Result<NormalizedTarget, NetGuardError> {
        let target = crate::normalize_url(raw, true)?;
        if target.host_is_ip.is_none()
            && crate::url_norm::is_private_hostname(&target.host_ascii)
            && !self.allows_private_hostname_for(&target.host_ascii, target.port)
        {
            return Err(NetGuardError::BlockedAddress(format!(
                "disallowed hostname `{}`",
                target.host_ascii
            )));
        }
        Ok(target)
    }

    pub fn is_self_blocked(&self, addr: SocketAddr) -> bool {
        let addr_ip = crate::canonicalize_ip(addr.ip());
        let embedded_v4s = match addr.ip() {
            std::net::IpAddr::V6(v6) => crate::ip::embedded_ipv4s(v6),
            std::net::IpAddr::V4(_) => [None; 9],
        };
        self.blocked_self.iter().any(|blocked| {
            if blocked.port() != addr.port() {
                return false;
            }
            let blocked_ip = crate::canonicalize_ip(blocked.ip());
            if blocked_ip == addr_ip
                || blocked_ip.is_unspecified()
                || addr_ip.is_unspecified()
                || (blocked_ip.is_loopback() && addr_ip.is_loopback())
            {
                return true;
            }
            embedded_v4s.into_iter().flatten().any(|v4| {
                let cand = std::net::IpAddr::V4(v4);
                blocked_ip == cand
                    || v4.is_unspecified()
                    || v4.octets()[0] == 0
                    || (blocked_ip.is_loopback() && v4.is_loopback())
            })
        })
    }

    fn matches_private_exception(&self, target: &NormalizedTarget, addr: SocketAddr) -> bool {
        self.private_exceptions.iter().any(|ex| {
            let host_matches = ex.host.eq_ignore_ascii_case(&target.host_ascii);
            let port_matches = ex.port == target.port && addr.port() == ex.port;
            let ip_matches = ex.allowed_ips.contains(&addr.ip());
            let path_matches = ex.path_prefixes.is_empty()
                || ex
                    .path_prefixes
                    .iter()
                    .any(|prefix| path_matches_prefix(target.url.path(), prefix));
            host_matches && port_matches && ip_matches && path_matches
        })
    }

    pub fn validate_target_for_method(
        &self,
        target: &NormalizedTarget,
        method: Option<&str>,
    ) -> Result<(), NetGuardError> {
        if is_cloud_metadata_host(&target.host_ascii) {
            return Err(NetGuardError::BlockedAddress(format!(
                "cloud metadata host `{}` is unconditionally blocked",
                target.host_ascii
            )));
        }
        if target.host_is_ip.is_none()
            && crate::url_norm::is_private_hostname(&target.host_ascii)
            && !self.allows_private_hostname_for(&target.host_ascii, target.port)
        {
            return Err(NetGuardError::BlockedAddress(format!(
                "disallowed hostname `{}`",
                target.host_ascii
            )));
        }
        if let Some(ip) = target.host_is_ip
            && is_cloud_metadata_ip(ip)
        {
            return Err(NetGuardError::BlockedAddress(format!(
                "cloud metadata ip `{ip}` is unconditionally blocked"
            )));
        }
        if let Some(allowed) = &self.allowed_origins {
            let matching_scopes: Vec<&OriginScope> = allowed
                .iter()
                .filter(|scope| {
                    scope.origin == target.origin
                        && (scope.path_prefixes.is_empty()
                            || scope
                                .path_prefixes
                                .iter()
                                .any(|prefix| path_matches_prefix(target.url.path(), prefix)))
                })
                .collect();
            if matching_scopes.is_empty() {
                return Err(NetGuardError::BlockedAddress(format!(
                    "target `{}` is outside allowed origin scopes",
                    target.origin
                )));
            }
            if let Some(m) = method
                && !matching_scopes.iter().any(|scope| {
                    scope.methods.as_ref().is_none_or(|ms| {
                        ms.iter().any(|allowed_m| allowed_m.eq_ignore_ascii_case(m))
                    })
                })
            {
                return Err(NetGuardError::MethodNotAllowed(m.to_string()));
            }
        }
        Ok(())
    }

    pub fn validate_target(&self, target: &NormalizedTarget) -> Result<(), NetGuardError> {
        self.validate_target_for_method(target, None)
    }

    pub fn validate_redirect_hop_for_method(
        &self,
        from: &NormalizedTarget,
        to: &NormalizedTarget,
        method: Option<&str>,
    ) -> Result<(), NetGuardError> {
        if from.origin.scheme == crate::Scheme::Https && to.origin.scheme == crate::Scheme::Http {
            return Err(NetGuardError::HttpsDowngradeBlocked);
        }
        self.validate_target_for_method(to, method)
    }

    pub fn validate_redirect_hop(
        &self,
        from: &NormalizedTarget,
        to: &NormalizedTarget,
    ) -> Result<(), NetGuardError> {
        self.validate_redirect_hop_for_method(from, to, None)
    }

    pub fn validate_socket_addr(
        &self,
        target: &NormalizedTarget,
        addr: SocketAddr,
    ) -> Result<(), NetGuardError> {
        self.validate_target(target)?;
        if is_cloud_metadata_ip(addr.ip()) {
            return Err(NetGuardError::BlockedAddress(format!(
                "cloud metadata ip `{}` is unconditionally blocked",
                addr.ip()
            )));
        }
        if self.is_self_blocked(addr) {
            return Err(NetGuardError::BlockedAddress(format!(
                "self-api address `{addr}` is unconditionally blocked"
            )));
        }
        if crate::has_unspecified_or_zero_ip(addr.ip()) {
            return Err(NetGuardError::BlockedAddress(format!(
                "unspecified target address `{addr}` is unconditionally blocked"
            )));
        }
        if is_global_unicast(addr.ip()) {
            return Ok(());
        }
        if self.test_loopback_sockets.contains(&addr) {
            return Ok(());
        }
        if self.matches_private_exception(target, addr) {
            return Ok(());
        }
        Err(NetGuardError::BlockedAddress(format!(
            "non-global target address `{addr}` blocked"
        )))
    }
}
