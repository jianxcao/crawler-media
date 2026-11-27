use crate::Invocation;
use domain::{Site, Torrent};
use indexer::{FetchMethod, FetchRequest, IndexerError, ProviderSource, ResolvedDownload};
use plugin_api::{
    self as api, ErrorCode, ErrorDetail, Failure, HttpTarget, TorrentRaw, UpstreamKind,
};
use std::fmt::Write as _;

pub(crate) fn effective_plugin_generation(source: &ProviderSource, call_generation: u64) -> u64 {
    match source {
        ProviderSource::PluginWasm {
            plugin_generation, ..
        } => *plugin_generation,
        _ => call_generation,
    }
}

pub(crate) fn map_torrent(site: &Site, raw: TorrentRaw) -> Torrent {
    Torrent {
        site_id: site.id,
        title: raw.title,
        enclosure: raw.download_ref,
        size_bytes: Some(raw.size),
        seeders: raw.seeders,
        free: raw.free.unwrap_or(false),
        hr: raw.hr,
        imdb_id: None,
        id: Some(raw.provider_local_id),
        leechers: raw.leechers,
        snatched: None,
        upload_time: raw.published_at.map(|ts| ts.to_string()),
        detail_url: None,
        category: raw.category,
        poster_url: None,
    }
}

fn validate_download_headers(headers: &[api::Header]) -> Result<(), IndexerError> {
    for header in headers {
        if header.name.is_empty()
            || header
                .name
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || header.value.chars().any(char::is_control)
        {
            return Err(IndexerError::InvalidInput(
                "invalid characters in download header".into(),
            ));
        }
        let lower = header.name.to_ascii_lowercase();
        if crate::http_site_ports::FORBIDDEN_REQUEST_HEADERS.contains(&lower.as_str()) {
            return Err(IndexerError::InvalidInput(format!(
                "forbidden download header `{}`",
                header.name
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_public_download_url(raw: &str) -> Result<(), IndexerError> {
    if raw.is_empty()
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
    {
        return Err(IndexerError::InvalidInput(
            "public download URL must not be empty or contain backslashes, whitespace, or control characters".into(),
        ));
    }
    let lower = raw.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else {
        return Err(IndexerError::InvalidInput(
            "public download URL must use http:// or https://".into(),
        ));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains('*') {
        return Err(IndexerError::InvalidInput(
            "public download URL authority is empty or contains userinfo/wildcard".into(),
        ));
    }
    let path = rest[authority.len()..]
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    if crate::http_site_ports::has_path_traversal(path) {
        return Err(IndexerError::InvalidInput(
            "public download URL contains path traversal".into(),
        ));
    }
    net_guard::normalize_url(raw, true)
        .map(|_| ())
        .map_err(|e| IndexerError::InvalidInput(e.to_string()))
}

fn urls_share_origin(a: &str, b: &str) -> bool {
    matches!(
        (
            net_guard::normalize_url(a, true),
            net_guard::normalize_url(b.trim(), true),
        ),
        (Ok(ta), Ok(tb)) if ta.origin == tb.origin
    )
}

fn validate_site_relative_path(path: &str) -> Result<(), IndexerError> {
    crate::http_site_ports::check_site_relative_path(path)
        .map_err(|msg| IndexerError::InvalidInput(msg.into()))
}

fn validate_download_auth_slot(
    site: &Site,
    provider_id: &str,
    site_auth_granted: bool,
    inv: &Invocation,
    auth_slot: Option<&str>,
) -> Result<bool, IndexerError> {
    let Some(slot) = auth_slot else {
        return Ok(false);
    };
    if !site_auth_granted {
        return Err(IndexerError::UnsupportedCapability {
            provider_id: provider_id.into(),
            capability: "http:site-auth".into(),
        });
    }
    if inv
        .credential_epoch
        .as_deref()
        .is_none_or(|e| e.trim().is_empty())
    {
        return Err(IndexerError::InvalidInput(
            "http:site-auth requires bound credential_epoch".into(),
        ));
    }
    let trimmed = slot.trim();
    if trimmed.is_empty() {
        return Err(IndexerError::InvalidInput(
            "auth_slot must not be empty".into(),
        ));
    }
    if trimmed
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("site:"))
    {
        let target_site = trimmed[5..].trim();
        if target_site.is_empty() {
            return Err(IndexerError::InvalidInput(
                "auth_slot must not be empty".into(),
            ));
        }
        if target_site != site.id.to_string() {
            return Err(IndexerError::InvalidInput(
                "auth_slot cannot reference another site".into(),
            ));
        }
    }
    Ok(true)
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

pub(crate) fn map_resolved_download(
    site: &Site,
    provider_id: &str,
    site_auth_granted: bool,
    inv: &Invocation,
    req: api::HttpRequest,
) -> Result<ResolvedDownload, IndexerError> {
    validate_download_headers(&req.headers)?;
    let is_json = req.headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case("content-type") && h.value.to_ascii_lowercase().contains("json")
    });
    let method = match req.method.trim().to_ascii_uppercase().as_str() {
        "POST" if is_json => FetchMethod::PostJson,
        "POST" => FetchMethod::PostForm,
        "GET" => FetchMethod::Get,
        other => {
            return Err(IndexerError::InvalidInput(format!(
                "unsupported download method `{other}`"
            )));
        }
    };
    let body = req.body.map(|b| String::from_utf8_lossy(&b).into_owned());
    match req.target {
        HttpTarget::PublicUrl(url) => {
            validate_public_download_url(&url)?;
            if method != FetchMethod::Get || body.is_some() || urls_share_origin(&url, &site.url) {
                return Ok(ResolvedDownload::Request(FetchRequest {
                    key: format!("download:{}", site.id),
                    method,
                    url,
                    body,
                    cookie: None,
                    api_key: None,
                    proxy: None,
                    cdp_url: None,
                }));
            }
            Ok(ResolvedDownload::Url(url))
        }
        HttpTarget::SiteRelative(target) => {
            validate_site_relative_path(&target.path)?;
            let with_auth = validate_download_auth_slot(
                site,
                provider_id,
                site_auth_granted,
                inv,
                target.auth_slot.as_deref(),
            )?;
            let base = site.url.trim_end_matches('/');
            let path = target.path.trim_start_matches('/');
            let url = if target.query.is_empty() {
                format!("{base}/{path}")
            } else {
                let query = target
                    .query
                    .into_iter()
                    .map(|h| {
                        format!(
                            "{}={}",
                            percent_encode_component(&h.name),
                            percent_encode_component(&h.value)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("&");
                format!("{base}/{path}?{query}")
            };
            Ok(ResolvedDownload::Request(FetchRequest {
                key: format!("download:{}", site.id),
                method,
                url,
                body,
                cookie: if with_auth { site.cookie.clone() } else { None },
                api_key: if with_auth {
                    site.api_key.clone()
                } else {
                    None
                },
                proxy: site.proxy.clone(),
                cdp_url: None,
            }))
        }
    }
}

pub(crate) fn map_failure(
    provider_id: &str,
    capability: &str,
    generation: u64,
    failure: Failure,
) -> IndexerError {
    match failure {
        Failure::Guest(err) => map_api_error(provider_id, capability, generation, err),
        Failure::Busy => IndexerError::BrowserBusy,
        Failure::InvalidInput(msg) => IndexerError::InvalidInput(msg),
        Failure::DeadlineExceeded => {
            IndexerError::UnknownOutcome("plugin invocation timed out".into())
        }
        Failure::Trap(msg) => IndexerError::UnknownOutcome(format!("plugin trapped: {msg}")),
        other => IndexerError::Fetch(other.to_string()),
    }
}

pub(crate) fn map_api_error(
    provider_id: &str,
    capability: &str,
    generation: u64,
    err: api::ApiError,
) -> IndexerError {
    match err.code {
        ErrorCode::Busy => IndexerError::BrowserBusy,
        ErrorCode::Unsupported => match err.detail {
            Some(ErrorDetail::BrowserIsolationUnavailable) => {
                IndexerError::BrowserSetup(err.message)
            }
            _ => IndexerError::UnsupportedCapability {
                provider_id: provider_id.into(),
                capability: capability.into(),
            },
        },
        ErrorCode::StaleGeneration => IndexerError::StaleGeneration {
            expected: generation,
            actual: generation,
        },
        ErrorCode::InvalidArgument | ErrorCode::StaleHandle => {
            IndexerError::InvalidInput(err.message)
        }
        ErrorCode::UnknownOutcome | ErrorCode::DeadlineExceeded => {
            IndexerError::UnknownOutcome(err.message)
        }
        ErrorCode::Upstream
            if matches!(err.detail, Some(ErrorDetail::Upstream(UpstreamKind::Parse))) =>
        {
            IndexerError::Parse(err.message)
        }
        _ => IndexerError::Fetch(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{api, map_api_error, validate_public_download_url};
    use indexer::IndexerError;
    use plugin_api::{ErrorCode, ErrorDetail};

    #[test]
    fn public_download_url_accepts_valid_hosts_ports_and_ipv6() {
        for url in [
            "https://cdn.example/a.torrent",
            "https://cdn.example:8443/a.torrent",
            "http://cdn.example:8080/a.torrent?x=1",
            "https://[::1]/a.torrent",
            "https://[::1]:8443/a.torrent",
            "https://[2001:db8::1]/a.torrent",
        ] {
            assert!(
                validate_public_download_url(url).is_ok(),
                "expected Ok for {url}"
            );
        }
    }

    #[test]
    fn public_download_url_rejects_userinfo_bad_ports_and_malformed_ipv6() {
        for url in [
            "https://user:pass@cdn.example/a.torrent",
            "https://trusted.com@evil.com/a.torrent",
            "https://@cdn.example/a.torrent",
            "https://cdn.example:abc/a.torrent",
            "https://cdn.example:65536/a.torrent",
            "https://cdn.example:0/a.torrent",
            "https://cdn.example:+80/a.torrent",
            "https://cdn.example:80:90/a.torrent",
            "https://[]/a.torrent",
            "https://[not-ipv6]/a.torrent",
            "https://[::1/a.torrent",
            "https://[::1]extra/a.torrent",
            "https://[::1]:abc/a.torrent",
            "https://cdn.example]/a.torrent",
            "https://:8080/a.torrent",
        ] {
            assert!(
                validate_public_download_url(url).is_err(),
                "expected Err for {url}"
            );
        }
    }

    #[test]
    fn map_api_error_preserves_unknown_outcome_with_blocked_address_detail() {
        let err = map_api_error(
            "plugin:org.example.maint/maint",
            "maintenance",
            3,
            api::ApiError {
                code: ErrorCode::UnknownOutcome,
                message: "redirect target blocked after mutation".into(),
                detail: Some(ErrorDetail::BlockedAddress),
                retry_after_ms: None,
                operation_id: None,
            },
        );
        assert!(
            matches!(err, IndexerError::UnknownOutcome(_)),
            "expected IndexerError::UnknownOutcome, got {err:?}"
        );
    }
}
