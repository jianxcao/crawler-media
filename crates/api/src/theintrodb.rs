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

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("TheIntroDB transport failed: {0}")]
    Transport(#[from] ureq::Error),
    #[error("TheIntroDB returned HTTP {0}")]
    Http(u16),
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
