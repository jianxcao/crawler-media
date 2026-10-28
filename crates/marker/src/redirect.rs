//! TTL cache for STRM redirectors (e.g. pan-302's `/api/302`).
//!
//! ffprobe/ffmpeg re-request the STRM URL on every seek, and each request
//! re-runs the redirector. Resolving the 302 once and handing the target to
//! ffmpeg removes those round trips. Any failure falls back to the original
//! URL, so the cache can only save work, never break a read.
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
        if let Some(target) = self.fresh(url) {
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
                tracing::debug!(%url, "STRM redirect resolved and cached");
                target
            }
            None => url.to_string(),
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
        let response = self
            .agent
            .get(url)
            .header("User-Agent", crate::active_probe_ua())
            .header("Range", "bytes=0-0")
            .call()
            .ok()?;
        if !response.status().is_redirection() {
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

/// Forget a cached target after a remote read failed.
pub fn invalidate_remote(url: &str) {
    REDIRECTS.invalidate(url);
}
