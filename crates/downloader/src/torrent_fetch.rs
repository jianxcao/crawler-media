use net_guard::{
    CredentialSlot, GuardedMethod, GuardedRequest, NetGuardClient, NetGuardError, NetGuardLimits,
    TargetPolicy,
};

use crate::{
    MAX_TORRENT_BYTES, TorrentParseError, VerifiedTorrentPayload, magnet_info_hash,
    parse_verified_torrent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionPurpose {
    Subscribe,
    Manual,
    Admit,
    Boost,
    Plugin,
}

#[derive(Clone, Debug)]
pub struct TorrentHttpSource {
    pub url: String,
    pub method: GuardedMethod,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub proxy: Option<String>,
    pub creds: Option<CredentialSlot>,
}

impl TorrentHttpSource {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: GuardedMethod::Get,
            headers: Vec::new(),
            body: None,
            proxy: None,
            creds: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TorrentSource {
    Http(TorrentHttpSource),
    Magnet {
        uri: String,
    },
    /// Opaque `download_ref` issued by a plugin indexer, which MUST be resolved
    /// into `Http` or `Magnet` by `download_service` before fetching.
    PluginRef(String),
}

impl TorrentSource {
    pub fn from_url(url: &str) -> Result<Self, TorrentFetchError> {
        let trimmed = url.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("magnet:") {
            return Ok(Self::Magnet {
                uri: trimmed.to_string(),
            });
        }
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Ok(Self::Http(TorrentHttpSource::get(trimmed.to_string())));
        }
        Err(TorrentFetchError::UnsupportedScheme)
    }

    pub fn from_site_enclosure(enclosure: &str) -> Result<Self, TorrentFetchError> {
        if let Ok(src) = Self::from_url(enclosure) {
            return Ok(src);
        }
        let trimmed = enclosure.trim();
        if !trimmed.is_empty() && !trimmed.contains(':') && !trimmed.starts_with("//") {
            return Ok(Self::PluginRef(trimmed.to_string()));
        }
        Err(TorrentFetchError::UnsupportedScheme)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedTorrent {
    TorrentFile(VerifiedTorrentPayload),
    Magnet { uri: String, info_hash: String },
}

#[derive(Debug, thiserror::Error)]
pub enum TorrentFetchError {
    #[error("unsupported download source scheme")]
    UnsupportedScheme,
    #[error("invalid magnet uri: missing or conflicting btih info hash")]
    InvalidMagnet,
    #[error("unknown-size magnet uri cannot be submitted for auto-boost")]
    UnknownMagnetSizeForBoost,
    #[error("http fetch returned non-success status {status} for {final_url}")]
    HttpStatus { status: u16, final_url: String },
    #[error("torrent response exceeds {limit} byte limit")]
    TooLarge { limit: u64 },
    #[error("torrent fetch outcome unknown after mutating request: {0}")]
    UnknownOutcome(String),
    #[error("transient torrent fetch failure: {0}")]
    Transient(String),
    #[error("network guard rejected torrent fetch: {0}")]
    NetGuard(#[source] NetGuardError),
    #[error(transparent)]
    Parse(#[from] TorrentParseError),
}

pub fn torrent_target_policy() -> TargetPolicy {
    TargetPolicy::production().with_limits(NetGuardLimits::torrent())
}

pub fn with_torrent_limits(mut policy: TargetPolicy) -> TargetPolicy {
    policy.limits.max_response_bytes = NetGuardLimits::torrent().max_response_bytes;
    policy
}

pub fn fetch_verified_torrent(
    client: &NetGuardClient,
    source: &TorrentHttpSource,
    policy: &TargetPolicy,
) -> Result<VerifiedTorrentPayload, TorrentFetchError> {
    let lower = source.url.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(TorrentFetchError::UnsupportedScheme);
    }
    let max_cap = MAX_TORRENT_BYTES as u64;
    let cap = policy.limits.max_response_bytes.min(max_cap);
    let request = GuardedRequest {
        method: source.method,
        url: source.url.clone(),
        headers: source.headers.clone(),
        body: source.body.clone(),
        proxy: source.proxy.clone(),
        max_response_bytes: Some(cap),
    };
    let response = client
        .execute(request, policy, source.creds.as_ref())
        .map_err(|err| match err {
            NetGuardError::BodyTooLarge { limit } => TorrentFetchError::TooLarge { limit },
            other if other.is_post_send_unknown() => {
                TorrentFetchError::UnknownOutcome(other.to_string())
            }
            other => TorrentFetchError::NetGuard(other),
        })?;
    if response
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("cf-mitigated") && !v.trim().is_empty())
    {
        return Err(TorrentFetchError::Transient(format!(
            "cloudflare challenge header (cf-mitigated) for {}",
            redact_fetch_url(&response.final_url)
        )));
    }
    if !response.is_success() {
        return Err(TorrentFetchError::HttpStatus {
            status: response.status,
            final_url: redact_fetch_url(&response.final_url),
        });
    }
    if let Some(expected) = response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        && response.body.len() < expected
    {
        return Err(TorrentFetchError::Transient(format!(
            "truncated .torrent response ({}/{expected} bytes) for {}",
            response.body.len(),
            redact_fetch_url(&response.final_url)
        )));
    }
    match parse_verified_torrent(&response.body) {
        Ok(parsed) => Ok(parsed),
        Err(parse_err) => {
            if is_cloudflare_interstitial(&response.body) {
                return Err(TorrentFetchError::Transient(
                    "cloudflare interstitial HTML returned instead of .torrent".into(),
                ));
            }
            if response.body.first() == Some(&b'd')
                && matches!(parse_err, TorrentParseError::MalformedBencode { .. })
            {
                return Err(TorrentFetchError::Transient(
                    "truncated bencode dictionary in .torrent response".into(),
                ));
            }
            Err(TorrentFetchError::Parse(parse_err))
        }
    }
}

fn is_cloudflare_interstitial(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("Just a moment...")
        || text.contains("Attention Required! | Cloudflare")
        || text.contains("cf-browser-verification")
}

fn redact_fetch_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(scheme_sep) = trimmed.find("://") else {
        return "[redacted-url]".to_string();
    };
    let scheme = &trimmed[..scheme_sep + 3];
    let rest = &trimmed[scheme_sep + 3..];
    let without_qf = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, path) = match without_qf.find('/') {
        Some(idx) => (&without_qf[..idx], &without_qf[idx..]),
        None => (without_qf, ""),
    };
    let host_port = authority
        .rfind('@')
        .map_or(authority, |at| &authority[at + 1..]);
    if host_port.is_empty() {
        return "[redacted-url]".to_string();
    }
    let mut prev_secret_dir = false;
    let new_segs: Vec<String> = path
        .split('/')
        .map(|seg| {
            let lower = seg.to_ascii_lowercase();
            if prev_secret_dir {
                let is_structural = matches!(
                    lower.as_str(),
                    "download"
                        | "dl"
                        | "get"
                        | "feed"
                        | "torrents"
                        | "torrent"
                        | "index.php"
                        | "download.php"
                        | "torrents.php"
                ) || (!seg.is_empty()
                    && seg.bytes().all(|b| b.is_ascii_digit()));
                if is_structural {
                    return seg.to_string();
                }
                if seg.len() >= 8 {
                    prev_secret_dir = false;
                    return "[REDACTED]".to_string();
                }
            }
            prev_secret_dir = matches!(lower.as_str(), "rss" | "passkey" | "torznab");
            if let Some((digits, tail)) = seg.split_once('.')
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !tail.is_empty()
            {
                let lower_tail = tail.to_ascii_lowercase();
                if !matches!(
                    lower_tail.as_str(),
                    "torrent" | "rss" | "xml" | "json" | "html" | "php" | "txt"
                ) {
                    if let Some(mid) = lower_tail.strip_suffix(".torrent")
                        && !mid.is_empty()
                    {
                        return format!("{digits}.[REDACTED].torrent");
                    }
                    return format!("{digits}.[REDACTED]");
                }
            }
            seg.to_string()
        })
        .collect();
    format!("{scheme}{host_port}{}", new_segs.join("/"))
}

pub fn prepare_download_payload(
    client: &NetGuardClient,
    source: &TorrentSource,
    purpose: SubmissionPurpose,
    policy: &TargetPolicy,
) -> Result<PreparedTorrent, TorrentFetchError> {
    match source {
        TorrentSource::Magnet { uri } => {
            let info_hash = magnet_info_hash(uri).ok_or(TorrentFetchError::InvalidMagnet)?;
            if purpose == SubmissionPurpose::Boost {
                return Err(TorrentFetchError::UnknownMagnetSizeForBoost);
            }
            Ok(PreparedTorrent::Magnet {
                uri: uri.clone(),
                info_hash,
            })
        }
        TorrentSource::Http(http_src) => {
            let payload = fetch_verified_torrent(client, http_src, policy)?;
            Ok(PreparedTorrent::TorrentFile(payload))
        }
        TorrentSource::PluginRef(_) => Err(TorrentFetchError::UnsupportedScheme),
    }
}
