use crate::archive::validate_safe_rel_path;
use crate::manifest_components::{validate_component, validate_credential_binding};
use crate::ui_manifest::validate_ui_declaration;
use crate::{HttpDestinationManifest, LimitsManifest, PackageError, PackagePolicy, PluginManifest};
use std::collections::HashSet;
use url::Url;

const RESERVED_PLUGIN_IDS: &[&str] = &["installed", "repos", "developer", "new"];
const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

pub(crate) fn validate_manifest_rules(
    m: &PluginManifest,
    policy: &PackagePolicy,
) -> Result<(), PackageError> {
    validate_top_level(m, policy)?;
    validate_http_and_limits(m)?;
    validate_sites_list(&m.sites)?;

    if m.components.is_empty() && m.sites.is_empty() && m.ui.is_none() {
        return Err(PackageError::EmptyPackage);
    }
    if m.components.len() > 16 {
        return Err(PackageError::InvalidManifest(
            "components count exceeds max 16".into(),
        ));
    }

    let mut comp_ids = HashSet::new();
    let mut task_ids = HashSet::new();
    let mut needs_cred_binding = false;

    for comp in &m.components {
        validate_component(
            comp,
            &m.api,
            policy,
            &mut comp_ids,
            &mut task_ids,
            &mut needs_cred_binding,
        )?;
    }

    validate_credential_binding(m.credential_binding.as_deref(), needs_cred_binding)?;

    if let Some(ui) = &m.ui {
        validate_ui_declaration(ui, &task_ids, policy)?;
    }
    Ok(())
}

fn validate_top_level(m: &PluginManifest, policy: &PackagePolicy) -> Result<(), PackageError> {
    if m.manifest_version != 1 {
        return Err(PackageError::InvalidManifest(
            "manifest_version must be 1".into(),
        ));
    }
    validate_plugin_id(&m.id)?;
    validate_plain_text("name", &m.name, 120, false)?;
    validate_plain_text("license", &m.license, 64, false)?;
    validate_plain_text("description", &m.description, 500, true)?;
    if m.authors.is_empty() || m.authors.len() > 32 {
        return Err(PackageError::InvalidManifest(
            "authors must contain 1..=32 entries".into(),
        ));
    }
    for author in &m.authors {
        validate_plain_text("authors[]", author, 100, false)?;
    }
    if semver::Version::parse(&m.version).is_err() {
        return Err(PackageError::InvalidManifest(format!(
            "invalid exact SemVer version `{}`",
            m.version
        )));
    }
    if semver::Version::parse(&m.api).is_err() {
        return Err(PackageError::InvalidManifest(format!(
            "invalid exact SemVer api `{}`",
            m.api
        )));
    }
    if m.api != policy.api_version {
        return Err(PackageError::UnsupportedWorld {
            api: m.api.clone(),
            world: String::new(),
        });
    }
    validate_safe_rel_path(&m.config_schema, &m.config_schema)?;
    if m.state_schema < 1 {
        return Err(PackageError::InvalidManifest(
            "state_schema must be >= 1".into(),
        ));
    }
    Ok(())
}

fn validate_plugin_id(id: &str) -> Result<(), PackageError> {
    if !(3..=128).contains(&id.len()) {
        return Err(PackageError::InvalidManifest(
            "plugin id length must be 3..=128".into(),
        ));
    }
    if RESERVED_PLUGIN_IDS.contains(&id) {
        return Err(PackageError::InvalidManifest(format!(
            "plugin id `{id}` is reserved"
        )));
    }
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2 {
        return Err(PackageError::InvalidManifest(
            "plugin id must be reverse-domain with at least 2 dot-separated segments".into(),
        ));
    }
    for seg in segments {
        if seg.is_empty()
            || seg.starts_with('-')
            || seg.ends_with('-')
            || !seg
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(PackageError::InvalidManifest(format!(
                "invalid segment `{seg}` in plugin id `{id}`"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_plain_text(
    field: &str,
    val: &str,
    max_len: usize,
    allow_newlines: bool,
) -> Result<(), PackageError> {
    let trimmed = val.trim();
    if trimmed.is_empty() || val.chars().count() > max_len {
        return Err(PackageError::InvalidManifest(format!(
            "field `{field}` must be 1..={max_len} non-whitespace characters"
        )));
    }
    if contains_html_tag(val) {
        return Err(PackageError::InvalidManifest(format!(
            "field `{field}` must not contain HTML"
        )));
    }
    if val.chars().any(|c| {
        if allow_newlines && (c == '\n' || c == '\r' || c == '\t') {
            false
        } else {
            c.is_control()
        }
    }) {
        return Err(PackageError::InvalidManifest(format!(
            "field `{field}` contains control characters"
        )));
    }
    Ok(())
}

fn contains_html_tag(s: &str) -> bool {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'<'
            && let Some(&next) = bytes.get(i + 1)
            && (next.is_ascii_alphabetic() || matches!(next, b'/' | b'!' | b'?'))
        {
            return true;
        }
    }
    false
}

fn validate_http_and_limits(m: &PluginManifest) -> Result<(), PackageError> {
    for dest in &m.http {
        validate_http_dest(dest)?;
    }
    if let Some(limits) = &m.limits {
        validate_limits(limits)?;
    }
    Ok(())
}

fn validate_http_dest(dest: &HttpDestinationManifest) -> Result<(), PackageError> {
    validate_http_origin(&dest.origin)?;
    validate_http_path_prefix(&dest.path_prefix)?;
    if dest.methods.is_empty() {
        return Err(PackageError::InvalidManifest(
            "http destination methods must be non-empty".into(),
        ));
    }
    for method in &dest.methods {
        if !HTTP_METHODS.contains(&method.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "unsupported http method `{method}`"
            )));
        }
    }
    Ok(())
}

fn validate_http_origin(origin: &str) -> Result<(), PackageError> {
    if origin.chars().any(is_invisible_or_format_char) {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` must not contain whitespace, control, or invisible characters"
        )));
    }
    let authority = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .ok_or_else(|| {
            PackageError::InvalidManifest(format!(
                "http origin `{origin}` must use lowercase http:// or https:// scheme://host[:port] syntax"
            ))
        })?;
    if authority.is_empty()
        || authority.ends_with(':')
        || authority.contains('/')
        || authority.contains('\\')
        || authority.contains('@')
        || authority.contains('?')
        || authority.contains('#')
    {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` must be a bare scheme://host[:port] origin without path, backslash, userinfo, query, or fragment"
        )));
    }
    let (raw_host, raw_port) = split_authority_host_port(authority, origin)?;
    if let Some(p) = raw_port
        && (p.is_empty()
            || !p.bytes().all(|b| b.is_ascii_digit())
            || p.starts_with('0')
            || !p.parse::<u16>().is_ok_and(|n| n > 0))
    {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` has invalid port `{p}`"
        )));
    }
    let url = Url::parse(origin).map_err(|e| {
        PackageError::InvalidManifest(format!("invalid http origin `{origin}`: {e}"))
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` must not contain credentials"
        )));
    }
    if url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || url.port() == Some(0)
    {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` must be an origin without path/query/fragment/port-0"
        )));
    }
    let Some(host_str) = url.host_str() else {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` missing host"
        )));
    };
    if host_str.contains('*') || host_str.ends_with('.') || raw_host.ends_with('.') {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` has invalid or wildcard/trailing-dot host"
        )));
    }
    validate_canonical_origin_host(origin, raw_host, host_str)?;
    if is_forbidden_origin_host(url.host()) {
        return Err(PackageError::InvalidManifest(format!(
            "http origin `{origin}` targets forbidden link-local, unspecified, multicast, or metadata host"
        )));
    }
    Ok(())
}

fn split_authority_host_port<'a>(
    authority: &'a str,
    origin: &str,
) -> Result<(&'a str, Option<&'a str>), PackageError> {
    if authority.starts_with('[') {
        let Some(close) = authority.find(']') else {
            return Err(PackageError::InvalidManifest(format!(
                "http origin `{origin}` has unclosed IPv6 host bracket"
            )));
        };
        let raw_host = &authority[..=close];
        let rest = &authority[close + 1..];
        if rest.is_empty() {
            Ok((raw_host, None))
        } else if let Some(port) = rest.strip_prefix(':') {
            Ok((raw_host, Some(port)))
        } else {
            Err(PackageError::InvalidManifest(format!(
                "http origin `{origin}` has malformed IPv6 authority"
            )))
        }
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        if h.contains(':') {
            return Err(PackageError::InvalidManifest(format!(
                "http origin `{origin}` has unbracketed IPv6 or extra colon"
            )));
        }
        Ok((h, Some(p)))
    } else {
        Ok((authority, None))
    }
}

fn validate_canonical_origin_host(
    origin: &str,
    raw_host: &str,
    host_str: &str,
) -> Result<(), PackageError> {
    if raw_host.is_ascii() && !raw_host.starts_with('[') {
        if !raw_host.eq_ignore_ascii_case(host_str) {
            return Err(PackageError::InvalidManifest(format!(
                "http origin `{origin}` must use canonical host `{host_str}`"
            )));
        }
    } else if !raw_host.is_ascii() {
        if raw_host.starts_with('[') {
            return Err(PackageError::InvalidManifest(format!(
                "http origin `{origin}` has non-ASCII bracketed host"
            )));
        }
        let raw_labels: Vec<&str> = raw_host.split('.').collect();
        let ascii_labels: Vec<&str> = host_str.split('.').collect();
        if raw_labels.len() != ascii_labels.len()
            || raw_labels
                .iter()
                .zip(ascii_labels.iter())
                .any(|(r, a)| !r.is_ascii() && !a.starts_with("xn--"))
        {
            return Err(PackageError::InvalidManifest(format!(
                "http origin `{origin}` contains disallowed Unicode compatibility or ignorable host characters"
            )));
        }
    }
    Ok(())
}

fn validate_http_path_prefix(prefix: &str) -> Result<(), PackageError> {
    if !prefix.starts_with('/')
        || prefix.contains("..")
        || prefix.contains('?')
        || prefix.contains('#')
        || prefix.contains('\\')
        || prefix.chars().any(is_invisible_or_format_char)
    {
        return Err(PackageError::InvalidManifest(format!(
            "invalid http path_prefix `{prefix}`"
        )));
    }
    Ok(())
}

fn is_invisible_or_format_char(c: char) -> bool {
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

fn validate_limits(l: &LimitsManifest) -> Result<(), PackageError> {
    if let Some(mem) = l.memory_mib
        && !(1..=256).contains(&mem)
    {
        return Err(PackageError::InvalidManifest(
            "limits.memory_mib must be in 1..=256".into(),
        ));
    }
    for (name, val) in [
        ("fuel_limit", l.fuel_limit),
        ("io_ms", l.io_ms),
        ("wall_ms", l.wall_ms),
        ("http_calls", l.http_calls.map(u64::from)),
        ("response_bytes", l.response_bytes),
        ("kv_bytes", l.kv_bytes),
    ] {
        if let Some(0) = val {
            return Err(PackageError::InvalidManifest(format!(
                "limits.{name} must be > 0"
            )));
        }
    }
    Ok(())
}

fn validate_sites_list(sites: &[String]) -> Result<(), PackageError> {
    let mut seen = HashSet::new();
    for path in sites {
        validate_safe_rel_path(path, path)?;
        if !(path.ends_with(".yaml") || path.ends_with(".yml")) {
            return Err(PackageError::InvalidManifest(format!(
                "site profile `{path}` must have .yaml or .yml extension"
            )));
        }
        if !seen.insert(path.as_str()) {
            return Err(PackageError::InvalidManifest(format!(
                "duplicate site path `{path}`"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_slug_id(field: &str, id: &str, max_len: usize) -> Result<(), PackageError> {
    if id.is_empty()
        || id.len() > max_len
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(PackageError::InvalidManifest(format!(
            "invalid {field} `{id}`: expected 1..={max_len} chars of [a-z0-9-]"
        )));
    }
    Ok(())
}

fn is_forbidden_ipv4(v4: std::net::Ipv4Addr) -> bool {
    v4.is_link_local()
        || v4.is_unspecified()
        || v4.octets()[0] == 0
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.octets() == [100, 100, 100, 200]
        || v4.octets() == [168, 63, 129, 16]
}

fn ipv4s_from_nat64(v6: std::net::Ipv6Addr) -> [Option<std::net::Ipv4Addr>; 4] {
    let segs = v6.segments();
    if segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2..6] == [0, 0, 0, 0] {
        return [
            Some(std::net::Ipv4Addr::new(
                (segs[6] >> 8) as u8,
                segs[6] as u8,
                (segs[7] >> 8) as u8,
                segs[7] as u8,
            )),
            None,
            None,
            None,
        ];
    }
    if segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2] == 0x0001 {
        let p96 = Some(std::net::Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            segs[6] as u8,
            (segs[7] >> 8) as u8,
            segs[7] as u8,
        ));
        let u_zero = (segs[4] >> 8) == 0;
        let p64 = u_zero.then(|| {
            std::net::Ipv4Addr::new(
                segs[4] as u8,
                (segs[5] >> 8) as u8,
                segs[5] as u8,
                (segs[6] >> 8) as u8,
            )
        });
        let p56 = u_zero.then(|| {
            std::net::Ipv4Addr::new(
                segs[3] as u8,
                segs[4] as u8,
                (segs[5] >> 8) as u8,
                segs[5] as u8,
            )
        });
        let p48 = u_zero.then(|| {
            std::net::Ipv4Addr::new(
                (segs[3] >> 8) as u8,
                segs[3] as u8,
                segs[4] as u8,
                (segs[5] >> 8) as u8,
            )
        });
        return [p96, p64, p56, p48];
    }
    [None; 4]
}

fn ipv4_from_6to4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let segs = v6.segments();
    (segs[0] == 0x2002).then(|| {
        std::net::Ipv4Addr::new(
            (segs[1] >> 8) as u8,
            segs[1] as u8,
            (segs[2] >> 8) as u8,
            segs[2] as u8,
        )
    })
}

fn ipv4s_from_teredo(v6: std::net::Ipv6Addr) -> Option<(std::net::Ipv4Addr, std::net::Ipv4Addr)> {
    let segs = v6.segments();
    (segs[0] == 0x2001 && segs[1] == 0x0000).then(|| {
        let srv = std::net::Ipv4Addr::new(
            (segs[2] >> 8) as u8,
            segs[2] as u8,
            (segs[3] >> 8) as u8,
            segs[3] as u8,
        );
        let c6 = !segs[6];
        let c7 = !segs[7];
        let cli = std::net::Ipv4Addr::new((c6 >> 8) as u8, c6 as u8, (c7 >> 8) as u8, c7 as u8);
        (srv, cli)
    })
}

fn is_forbidden_origin_host(host: Option<url::Host<&str>>) -> bool {
    match host {
        Some(url::Host::Ipv4(v4)) => is_forbidden_ipv4(v4),
        Some(url::Host::Ipv6(v6)) => {
            let segs = v6.segments();
            let is_ec2_imds = segs[0] == 0xfd00 && segs[1] == 0x0ec2;
            let is_v4_compat_forbidden = segs[..6] == [0; 6]
                && !v6.is_loopback()
                && v6.to_ipv4().is_some_and(is_forbidden_ipv4);
            v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
                || is_ec2_imds
                || v6.to_ipv4_mapped().is_some_and(is_forbidden_ipv4)
                || is_v4_compat_forbidden
                || ipv4s_from_nat64(v6)
                    .into_iter()
                    .flatten()
                    .any(is_forbidden_ipv4)
                || ipv4_from_6to4(v6).is_some_and(is_forbidden_ipv4)
                || ipv4s_from_teredo(v6)
                    .is_some_and(|(srv, cli)| is_forbidden_ipv4(srv) || is_forbidden_ipv4(cli))
        }
        Some(url::Host::Domain(domain)) => {
            let d = domain.trim_end_matches('.').to_ascii_lowercase();
            matches!(
                d.as_str(),
                "metadata"
                    | "instance-data"
                    | "metadata.google.internal"
                    | "metadata.goog"
                    | "metadata.azure.internal"
            ) || d.ends_with(".metadata.google.internal")
                || d.ends_with(".metadata.goog")
                || d.ends_with(".metadata.azure.internal")
        }
        None => true,
    }
}
