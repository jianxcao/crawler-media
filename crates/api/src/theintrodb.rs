//! TheIntroDB v4 read API. Cloud timestamps remain distinct from file-local verification.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Segment {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct EpisodeSegments {
    #[serde(rename = "type")]
    media_type: String,
    season: u32,
    episode: u32,
    group_duration_ms: Option<i64>,
    #[serde(default)]
    intro: Vec<Segment>,
    #[serde(default)]
    credits: Vec<Segment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkerResult {
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
    pub group_duration_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitScope {
    Burst,
    DailyQuota,
    SpecificMedia,
}

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("TheIntroDB transport failed: {0}")]
    Transport(#[from] ureq::Error),
    #[error("TheIntroDB returned HTTP {0}")]
    Http(u16),
    #[error("TheIntroDB rate limited ({scope:?}), retry after {retry_after_ms}ms")]
    RateLimited {
        scope: RateLimitScope,
        retry_after_ms: i64,
    },
    #[error("TheIntroDB rejected credentials with HTTP {0}")]
    Unauthorized(u16),
    #[error("TheIntroDB could not resolve the requested episode")]
    EpisodeNotFound,
    #[error("TheIntroDB could not resolve the requested Media")]
    MediaNotFound,
    #[error("TheIntroDB returned an invalid response: {0}")]
    Response(#[from] serde_json::Error),
    #[error("TheIntroDB response does not match the requested episode")]
    Coordinates,
    #[error("TheIntroDB returned invalid segment boundaries")]
    Boundaries,
}

#[derive(Clone)]
pub struct TheIntroDbClient {
    api_key: Option<String>,
    base_url: String,
    configuration_revision: i64,
}

impl std::fmt::Debug for TheIntroDbClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TheIntroDbClient")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl Default for TheIntroDbClient {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TheIntroDbClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            base_url: "https://api.theintrodb.org/v4".into(),
            configuration_revision: 0,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub(crate) fn with_configuration_revision(mut self, revision: i64) -> Self {
        self.configuration_revision = revision;
        self
    }

    pub(crate) fn cache_scope(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.base_url.hash(&mut hasher);
        self.api_key.hash(&mut hasher);
        self.configuration_revision.hash(&mut hasher);
        hasher.finish()
    }

    /// Select a release by this file's duration. The API may fall back to an
    /// unrelated popular release, so callers must check `group_duration_ms`.
    pub fn get_episode_markers(
        &self,
        tmdb_id: &str,
        season: u32,
        episode: u32,
        duration_ms: Option<i64>,
    ) -> Result<Option<MarkerResult>, LookupError> {
        let url = format!("{}/media", self.base_url.trim_end_matches('/'));
        tracing::info!(
            tmdb_id,
            season,
            episode,
            duration_ms,
            "【片头片尾】查询 TheIntroDB v4 云端时间戳"
        );
        let resp = crate::http_agent::call_url(&url, |agent| {
            let mut request = agent
                .get(&url)
                .config()
                .http_status_as_error(false)
                .build()
                .query("tmdb_id", tmdb_id)
                .query("season", season.to_string())
                .query("episode", episode.to_string())
                .query("details", "true")
                .query("merge_unknown", "false")
                .header("Accept", "application/json");
            if let Some(duration) = duration_ms.filter(|duration| *duration > 0) {
                request = request.query("duration_ms", duration.to_string());
            }
            if let Some(key) = self
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
            {
                request = request.header("Authorization", format!("Bearer {key}"));
            }
            request.call()
        })?;
        let status = resp.status().as_u16();
        if status != 200 {
            if status == 429 {
                let headers = resp.headers().clone();
                let body_bytes = resp.into_body().read_to_vec().unwrap_or_default();
                let code = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                    .ok()
                    .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string));
                return Err(classify_429(&headers, code.as_deref()));
            }
            if status == 401 || status == 403 {
                return Err(LookupError::Unauthorized(status));
            }
            if status == 404 {
                let body: serde_json::Value = resp.into_body().read_json()?;
                match body.get("code").and_then(serde_json::Value::as_str) {
                    Some("no_submissions") => {
                        tracing::info!(
                            tmdb_id,
                            season,
                            episode,
                            code = "no_submissions",
                            "【片头片尾】TheIntroDB 尚无该集提交"
                        );
                        return Ok(None);
                    }
                    Some("episode_not_found") => return Err(LookupError::EpisodeNotFound),
                    Some("media_not_found") => return Err(LookupError::MediaNotFound),
                    _ => {}
                }
            }
            return Err(LookupError::Http(status));
        }
        let body = resp.into_body().read_to_vec()?;
        let response: EpisodeSegments = serde_json::from_slice(&body)?;
        if response.media_type != "tv" || response.season != season || response.episode != episode {
            return Err(LookupError::Coordinates);
        }
        let intro = single_range(&response.intro, false)?;
        let credits = single_range(&response.credits, true)?;
        if intro.is_none() && credits.is_none() {
            return Ok(None);
        }
        Ok(Some(MarkerResult {
            intro_start_ms: intro.map(|range| range.0),
            intro_end_ms: intro.and_then(|range| range.1),
            outro_start_ms: credits.map(|range| range.0),
            outro_end_ms: credits.and_then(|range| range.1),
            group_duration_ms: response.group_duration_ms,
        }))
    }
}

fn single_range(
    segments: &[Segment],
    credits: bool,
) -> Result<Option<(i64, Option<i64>)>, LookupError> {
    let [segment] = segments else {
        if !segments.is_empty() {
            tracing::warn!(
                count = segments.len(),
                credits,
                "TheIntroDB 多段范围不能表示为单个跳过区间，保留为候选"
            );
        }
        return Ok(None);
    };
    let start = match (credits, segment.start_ms) {
        (true, Some(0)) | (true, None) => return Ok(None),
        (_, start) => start.unwrap_or(0),
    };
    if !credits && segment.end_ms == Some(0) {
        return Ok(None);
    }
    if start < 0
        || segment.end_ms.is_some_and(|end| end <= start)
        || (!credits && segment.end_ms.is_none())
    {
        return Err(LookupError::Boundaries);
    }
    Ok(Some((start, segment.end_ms)))
}

fn header_u64(headers: &ureq::http::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|raw| raw.parse::<u64>().ok())
}

fn normalize_reset_secs(raw: u64, now_unix_secs: u64) -> u64 {
    if raw >= 1_000_000_000 {
        raw.saturating_sub(now_unix_secs)
    } else {
        raw
    }
}

fn classify_429(headers: &ureq::http::HeaderMap, code: Option<&str>) -> LookupError {
    let now_unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let retry_after = header_u64(headers, "retry-after");
    let specific_reset = header_u64(headers, "x-usagelimit-specificmedia-reset")
        .map(|s| normalize_reset_secs(s, now_unix_secs));
    let specific_rem = header_u64(headers, "x-usagelimit-specificmedia-remaining");
    let usage_reset =
        header_u64(headers, "x-usagelimit-reset").map(|s| normalize_reset_secs(s, now_unix_secs));
    let usage_rem = header_u64(headers, "x-usagelimit-remaining");
    let rate_reset =
        header_u64(headers, "x-ratelimit-reset").map(|s| normalize_reset_secs(s, now_unix_secs));

    let (scope, secs, min_ms, max_ms) = if code == Some("specific_media_usage_limit_exceeded")
        || specific_rem == Some(0)
        || (specific_reset.is_some()
            && code != Some("usage_limit_exceeded")
            && usage_rem != Some(0))
    {
        (
            RateLimitScope::SpecificMedia,
            specific_reset.or(retry_after).unwrap_or(3_600),
            60_000,
            86_400_000,
        )
    } else if code == Some("usage_limit_exceeded") || usage_rem == Some(0) || usage_reset.is_some()
    {
        (
            RateLimitScope::DailyQuota,
            usage_reset.or(retry_after).unwrap_or(3_600),
            60_000,
            86_400_000,
        )
    } else {
        (
            RateLimitScope::Burst,
            retry_after.or(rate_reset).unwrap_or(10),
            1_000,
            900_000,
        )
    };
    let retry_after_ms = secs.saturating_mul(1_000).clamp(min_ms, max_ms) as i64;
    LookupError::RateLimited {
        scope,
        retry_after_ms,
    }
}

#[cfg(test)]
mod debug_tests {
    use super::TheIntroDbClient;

    #[test]
    fn debug_never_prints_the_api_key() {
        let client = TheIntroDbClient::new(Some("theintrodb-key-secret".into()));
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("theintrodb-key-secret"));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("api.theintrodb.org"));
        assert!(format!("{:?}", TheIntroDbClient::default()).contains("None"));
    }
}
