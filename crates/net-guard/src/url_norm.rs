use std::fmt;
use std::net::IpAddr;

use crate::NetGuardError;
use crate::ip::{is_cloud_metadata_host, is_cloud_metadata_ip};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
}

impl Origin {
    pub fn new(scheme: Scheme, host: impl Into<String>, port: Option<u16>) -> Self {
        Self {
            scheme,
            host: host.into(),
            port: port.unwrap_or_else(|| scheme.default_port()),
        }
    }

    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.scheme.default_port() {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme.as_str(), self.authority())
    }
}

#[derive(Clone, Debug)]
pub struct NormalizedTarget {
    pub url: url::Url,
    pub origin: Origin,
    pub path_and_query: String,
    pub host_is_ip: Option<IpAddr>,
    pub host_ascii: String,
    pub port: u16,
}

fn is_invisible_or_control(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFF}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E01EF}'
        )
}

fn check_raw_url_safety(raw: &str) -> Result<(), NetGuardError> {
    if raw.is_empty() {
        return Err(NetGuardError::InvalidUrl("url must not be empty".into()));
    }
    if raw.chars().any(is_invisible_or_control) {
        return Err(NetGuardError::InvalidUrl(
            "url contains whitespace, control, or invisible characters".into(),
        ));
    }
    if raw.contains('\\') {
        return Err(NetGuardError::InvalidUrl(
            "url must not contain backslashes".into(),
        ));
    }
    let Some((scheme_part, after_scheme)) = raw.split_once("://") else {
        return Err(NetGuardError::InvalidUrl(
            "url must include http:// or https://".into(),
        ));
    };
    if !scheme_part.eq_ignore_ascii_case("http") && !scheme_part.eq_ignore_ascii_case("https") {
        return Err(NetGuardError::InvalidUrl(format!(
            "unsupported url scheme `{scheme_part}`"
        )));
    }
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.contains('*') {
        return Err(NetGuardError::InvalidUrl(
            "url authority is empty or contains userinfo/wildcard".into(),
        ));
    }
    let path_and_tail = &after_scheme[authority_end..];
    let raw_path = path_and_tail.split(['?', '#']).next().unwrap_or_default();
    let lower_path = raw_path.to_ascii_lowercase();
    if lower_path.contains("%5c")
        || lower_path.contains("%2f")
        || lower_path.split('/').any(|seg| {
            let core = seg.split(';').next().unwrap_or(seg);
            let core = core.split("%3b").next().unwrap_or(core);
            matches!(core, ".." | "%2e%2e" | ".%2e" | "%2e." | "%2e")
                || (core == "." && core != seg)
        })
    {
        return Err(NetGuardError::InvalidUrl(
            "url path contains traversal segments".into(),
        ));
    }
    Ok(())
}

pub(crate) fn is_private_hostname(host: &str) -> bool {
    !host.contains('.')
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
}

fn is_disallowed_hostname(host: &str, allow_private_hostnames: bool) -> bool {
    if is_cloud_metadata_host(host) {
        return true;
    }
    if allow_private_hostnames {
        return false;
    }
    is_private_hostname(host)
}

pub fn normalize_url(
    raw: &str,
    allow_private_hostnames: bool,
) -> Result<NormalizedTarget, NetGuardError> {
    check_raw_url_safety(raw)?;

    let mut parsed = url::Url::parse(raw).map_err(|e| NetGuardError::InvalidUrl(e.to_string()))?;
    let scheme = match parsed.scheme() {
        "http" => Scheme::Http,
        "https" => Scheme::Https,
        other => {
            return Err(NetGuardError::InvalidUrl(format!(
                "unsupported url scheme `{other}`"
            )));
        }
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(NetGuardError::InvalidUrl(
            "url must not contain userinfo".into(),
        ));
    }
    if parsed.port() == Some(0) {
        return Err(NetGuardError::InvalidUrl("port 0 is forbidden".into()));
    }
    parsed.set_fragment(None);

    let (host_is_ip, host_ascii) = match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            let ip = IpAddr::V4(v4);
            if is_cloud_metadata_ip(ip) {
                return Err(NetGuardError::BlockedAddress(format!(
                    "cloud metadata ip `{ip}` is blocked"
                )));
            }
            (Some(ip), v4.to_string())
        }
        Some(url::Host::Ipv6(v6)) => {
            let ip = IpAddr::V6(v6);
            if is_cloud_metadata_ip(ip) {
                return Err(NetGuardError::BlockedAddress(format!(
                    "cloud metadata ip `{ip}` is blocked"
                )));
            }
            (Some(ip), v6.to_string())
        }
        Some(url::Host::Domain(dom)) => {
            let stripped = dom.trim_end_matches('.');
            if stripped.is_empty() || stripped.contains('*') {
                return Err(NetGuardError::InvalidUrl("invalid domain host".into()));
            }
            let ascii = idna::domain_to_ascii(stripped)
                .map_err(|e| NetGuardError::InvalidUrl(format!("invalid IDNA host: {e:?}")))?
                .to_ascii_lowercase();
            if ascii.is_empty() || ascii.contains('*') {
                return Err(NetGuardError::InvalidUrl("invalid normalized host".into()));
            }
            if is_disallowed_hostname(&ascii, allow_private_hostnames) {
                return Err(NetGuardError::BlockedAddress(format!(
                    "disallowed hostname `{ascii}`"
                )));
            }
            parsed
                .set_host(Some(&ascii))
                .map_err(|e| NetGuardError::InvalidUrl(e.to_string()))?;
            match parsed.host() {
                Some(url::Host::Ipv4(v4)) => {
                    let ip = IpAddr::V4(v4);
                    if is_cloud_metadata_ip(ip) {
                        return Err(NetGuardError::BlockedAddress(format!(
                            "cloud metadata ip `{ip}` is blocked"
                        )));
                    }
                    (Some(ip), v4.to_string())
                }
                Some(url::Host::Ipv6(v6)) => {
                    let ip = IpAddr::V6(v6);
                    if is_cloud_metadata_ip(ip) {
                        return Err(NetGuardError::BlockedAddress(format!(
                            "cloud metadata ip `{ip}` is blocked"
                        )));
                    }
                    (Some(ip), v6.to_string())
                }
                _ => (None, ascii),
            }
        }
        None => {
            return Err(NetGuardError::InvalidUrl("url missing host".into()));
        }
    };

    let port = parsed
        .port_or_known_default()
        .unwrap_or(scheme.default_port());
    if parsed.port() == Some(scheme.default_port()) {
        let _ = parsed.set_port(None);
    }

    let path_and_query = match parsed.query() {
        Some(q) => format!("{}?{q}", parsed.path()),
        None => parsed.path().to_string(),
    };

    let origin = Origin::new(scheme, host_ascii.clone(), Some(port));
    Ok(NormalizedTarget {
        url: parsed,
        origin,
        path_and_query,
        host_is_ip,
        host_ascii,
        port,
    })
}
