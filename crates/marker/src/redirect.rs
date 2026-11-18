//! TTL cache for STRM redirectors (e.g. pan-302's `/api/302`).
//!
//! ffprobe/ffmpeg re-request the STRM URL on every seek, and each request
//! re-runs the redirector. Resolving the 302 once and handing the target to
//! ffmpeg removes those round trips. Successful direct responses are cached too,
//! so a source without redirects is not preflighted on every read. Failures
//! fall back to the original URL and are never cached as successful lookups.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Signed CDN targets expire; keep entries well inside a typical signature life.
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ENTRIES: usize = 4096;

static REDIRECTS: LazyLock<RedirectCache> = LazyLock::new(|| RedirectCache::new(DEFAULT_TTL));

pub struct RedirectCache {
    ttl: Duration,
    entries: Mutex<HashMap<String, (String, Instant)>>,
    agent: ureq::Agent,
}

impl RedirectCache {
    pub fn new(ttl: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(RESOLVE_TIMEOUT))
            .build()
            .into();
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
            agent,
        }
    }

    /// Redirect target for `url`, or `url` itself when it does not redirect
    /// or cannot be resolved right now.
    pub fn resolve(&self, url: &str) -> String {
        let started = Instant::now();
        if let Some(target) = self.fresh(url) {
            tracing::debug!(
                cache_hit = true,
                redirected = target != url,
                elapsed_ms = started.elapsed().as_millis(),
                "STRM target resolution completed"
            );
            return target;
        }
        match self.fetch_location(url) {
            Some(target) => {
                let mut entries = self.lock();
                if entries.len() >= MAX_ENTRIES {
                    let now = Instant::now();
                    entries.retain(|_, (_, expires)| *expires > now);
                    if entries.len() >= MAX_ENTRIES {
                        entries.clear();
                    }
                }
                entries.insert(url.to_string(), (target.clone(), Instant::now() + self.ttl));
                tracing::debug!(
                    cache_hit = false,
                    redirected = target != url,
                    elapsed_ms = started.elapsed().as_millis(),
                    "STRM target resolution completed"
                );
                target
            }
            None => {
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "STRM target resolution fell back to the original input"
                );
                url.to_string()
            }
        }
    }

    /// Drop a cached target (e.g. after the target URL failed).
    pub fn invalidate(&self, url: &str) {
        self.lock().remove(url);
    }

    fn fresh(&self, url: &str) -> Option<String> {
        let entries = self.lock();
        let (target, expires) = entries.get(url)?;
        (Instant::now() < *expires).then(|| target.clone())
    }

    fn fetch_location(&self, url: &str) -> Option<String> {
        let response = match self
            .agent
            .get(url)
            .header("User-Agent", crate::active_probe_ua())
            .header("Range", "bytes=0-0")
            .call()
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error_kind = ?std::mem::discriminant(&error),
                    "STRM target resolution failed; using original input");
                return None;
            }
        };
        if response.status().is_success() {
            return Some(url.to_string());
        }
        if !response.status().is_redirection() {
            tracing::warn!(
                status = response.status().as_u16(),
                "STRM target resolution returned an unexpected status; using original input"
            );
            return None;
        }
        let location = response.headers().get("location")?.to_str().ok()?.trim();
        (location.starts_with("http://") || location.starts_with("https://"))
            .then(|| location.to_string())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, Instant)>> {
        self.entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Process-wide resolution used for every remote ffprobe/ffmpeg input.
pub fn resolve_remote(url: &str) -> String {
    REDIRECTS.resolve(url)
}
