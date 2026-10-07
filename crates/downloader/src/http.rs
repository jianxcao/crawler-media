use std::net::IpAddr;
use std::time::Duration;

/// Build a downloader API client. Local services bypass inherited HTTP proxies;
/// remote endpoints retain the normal environment proxy configuration.
pub(crate) fn agent_for_url(url: &str, http_status_as_error: bool) -> ureq::Agent {
    let mut config = ureq::config::Config::builder()
        .http_status_as_error(http_status_as_error)
        .timeout_global(Some(Duration::from_secs(5)));
    if is_local_endpoint(url) {
        config = config.proxy(None);
    }
    ureq::Agent::new_with_config(config.build())
}

fn is_local_endpoint(url: &str) -> bool {
    let Some((_, remainder)) = url.split_once("://") else {
        return false;
    };
    let authority = remainder.split('/').next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host == "host.docker.internal"
        || host.ends_with(".local")
        || !host.contains('.')
    {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => false,
    }
}
