//! TTL cache for STRM redirectors (e.g. pan-302's `/api/302`).
//!
//! ffprobe/ffmpeg re-request the STRM URL on every seek, and each request
//! re-runs the redirector. Resolving the 302 once and handing the target to
//! ffmpeg removes those round trips. Successful direct responses are cached too,
//! so a source without redirects is not preflighted on every read. Failures
//! fall back to the original URL and are never cached as successful lookups.
//!
//! Cache lifetime honors upstream `Cache-Control` (`no-store`, `no-cache`,
//! `max-age`, `s-maxage`), `Expires`, and signed CDN target expiry parameters,
//! always upper-bounded by the cache's configured TTL.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Upper bound on cached redirect entries when upstream supplies no tighter lifetime.
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const SIGNED_URL_SAFETY_MARGIN: Duration = Duration::from_secs(5);
const MAX_ENTRIES: usize = 4096;

static REDIRECTS: LazyLock<RedirectCache> = LazyLock::new(|| RedirectCache::new(DEFAULT_TTL));

struct ResolvedTarget {
    target: String,
    ttl: Option<Duration>,
}

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
            Some(ResolvedTarget { target, ttl }) => {
                let mut cached = false;
                if let Some(ttl) = ttl.filter(|d| !d.is_zero())
                    && let Some(expires) = Instant::now().checked_add(ttl)
                {
                    let mut entries = self.lock();
                    if entries.len() >= MAX_ENTRIES {
                        let now = Instant::now();
                        entries.retain(|_, (_, expires)| *expires > now);
                        if entries.len() >= MAX_ENTRIES {
                            entries.clear();
                        }
                    }
                    entries.insert(url.to_string(), (target.clone(), expires));
                    cached = true;
                }
                tracing::debug!(
                    cache_hit = false,
                    redirected = target != url,
                    cached,
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

    fn fetch_location(&self, url: &str) -> Option<ResolvedTarget> {
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
        let headers = response.headers();
        let cache_control = header_str(headers, "cache-control");
        let expires = header_str(headers, "expires");
        let date = header_str(headers, "date");
        if response.status().is_success() {
            let ttl = response_cache_ttl(
                self.ttl,
                cache_control,
                expires,
                date,
                None,
                SystemTime::now(),
            );
            return Some(ResolvedTarget {
                target: url.to_string(),
                ttl,
            });
        }
        if !response.status().is_redirection() {
            tracing::warn!(
                status = response.status().as_u16(),
                "STRM target resolution returned an unexpected status; using original input"
            );
            return None;
        }
        let location = header_str(headers, "location")?.trim();
        if !(location.starts_with("http://") || location.starts_with("https://")) {
            return None;
        }
        let ttl = response_cache_ttl(
            self.ttl,
            cache_control,
            expires,
            date,
            Some(location),
            SystemTime::now(),
        );
        Some(ResolvedTarget {
            target: location.to_string(),
            ttl,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, Instant)>> {
        self.entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn header_str<'a>(headers: &'a ureq::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// Compute effective cache TTL bounded by `max_ttl`. Returns `None` when caching is
/// prohibited or the target is already expired (or within safety margin).
fn response_cache_ttl(
    max_ttl: Duration,
    cache_control: Option<&str>,
    expires: Option<&str>,
    date: Option<&str>,
    target_url: Option<&str>,
    now: SystemTime,
) -> Option<Duration> {
    if max_ttl.is_zero() {
        return None;
    }
    let header_ttl = if let Some(cc) = cache_control {
        match parse_cache_control(cc) {
            CacheControlTtl::DoNotCache => return None,
            CacheControlTtl::MaxAge(age) => Some(age),
            CacheControlTtl::Unspecified => parse_expires_ttl(expires, date, now)?,
        }
    } else {
        parse_expires_ttl(expires, date, now)?
    };
    let mut ttl = header_ttl.map_or(max_ttl, |d| d.min(max_ttl));
    if let Some(signed_ttl) = target_url.and_then(|url| signed_url_ttl(url, now)) {
        ttl = ttl.min(signed_ttl?);
    }
    (!ttl.is_zero()).then_some(ttl)
}

enum CacheControlTtl {
    DoNotCache,
    MaxAge(Duration),
    Unspecified,
}

fn parse_cache_control(header: &str) -> CacheControlTtl {
    let mut max_age: Option<Duration> = None;
    for raw in header.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let (name, value) = match token.split_once('=') {
            Some((k, v)) => (k.trim(), Some(v.trim().trim_matches('"'))),
            None => (token, None),
        };
        if name.eq_ignore_ascii_case("no-store") || name.eq_ignore_ascii_case("no-cache") {
            return CacheControlTtl::DoNotCache;
        }
        if (name.eq_ignore_ascii_case("max-age") || name.eq_ignore_ascii_case("s-maxage"))
            && let Some(secs) = value.and_then(|v| v.parse::<u64>().ok())
        {
            if secs == 0 {
                return CacheControlTtl::DoNotCache;
            }
            let d = Duration::from_secs(secs);
            max_age = Some(max_age.map_or(d, |prev| prev.min(d)));
        }
    }
    match max_age {
        Some(d) => CacheControlTtl::MaxAge(d),
        None => CacheControlTtl::Unspecified,
    }
}

/// Returns `None` if `Expires` forbids caching (expired/invalid), `Some(None)` if absent,
/// or `Some(Some(ttl))` if valid future expiry.
fn parse_expires_ttl(
    expires: Option<&str>,
    date: Option<&str>,
    now: SystemTime,
) -> Option<Option<Duration>> {
    let Some(raw) = expires.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(None);
    };
    let Ok(exp_time) = httpdate::parse_http_date(raw) else {
        // RFC 9111 §5.3: invalid date strings (such as "0" or "-1") represent an expired date.
        return None;
    };
    let reference = date
        .map(str::trim)
        .and_then(|d| httpdate::parse_http_date(d).ok())
        .unwrap_or(now);
    let remaining = exp_time.duration_since(reference).ok()?;
    (!remaining.is_zero()).then_some(Some(remaining))
}

/// Inspect common signed-CDN expiry query params on a redirected `Location` URL.
/// Returns `None` when no signed-expiry parameter is present, `Some(None)` when expired
/// or within safety margin, or `Some(Some(remaining))` when still fresh.
fn signed_url_ttl(url: &str, now: SystemTime) -> Option<Option<Duration>> {
    let query = url.split_once('?')?.1.split('#').next()?;
    let mut amz_date: Option<&str> = None;
    let mut amz_expires: Option<u64> = None;
    let mut unix_expiry: Option<u64> = None;

    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("x-amz-date") {
            amz_date = Some(value);
        } else if key.eq_ignore_ascii_case("x-amz-expires") {
            amz_expires = value.parse::<u64>().ok();
        } else if (key.eq_ignore_ascii_case("expires")
            || key.eq_ignore_ascii_case("exp")
            || key.eq_ignore_ascii_case("expire"))
            && let Ok(ts) = value.parse::<u64>()
        {
            let secs = if ts > 1_000_000_000_000 {
                ts / 1000
            } else {
                ts
            };
            // Only treat plausible unix timestamps (>= 2001-09-09).
            if secs >= 1_000_000_000 {
                unix_expiry = Some(unix_expiry.map_or(secs, |prev| prev.min(secs)));
            }
        }
    }

    let mut limit: Option<Duration> = None;
    let mut found = false;
    if let Some(secs) = unix_expiry {
        found = true;
        let Some(rem) = remaining_until_expiry(UNIX_EPOCH, secs, now) else {
            return Some(None);
        };
        let safe = rem.saturating_sub(SIGNED_URL_SAFETY_MARGIN);
        if safe.is_zero() {
            return Some(None);
        }
        limit = Some(limit.map_or(safe, |prev| prev.min(safe)));
    }
    if let Some(exp_secs) = amz_expires {
        found = true;
        let rem = match amz_date.and_then(parse_amz_date) {
            Some(issued) => match remaining_until_expiry(issued, exp_secs, now) {
                Some(rem) => rem,
                None => return Some(None),
            },
            None => Duration::from_secs(exp_secs),
        };
        let safe = rem.saturating_sub(SIGNED_URL_SAFETY_MARGIN);
        if safe.is_zero() {
            return Some(None);
        }
        limit = Some(limit.map_or(safe, |prev| prev.min(safe)));
    }
    found.then_some(limit)
}

fn remaining_until_expiry(base: SystemTime, add_secs: u64, now: SystemTime) -> Option<Duration> {
    let Some(exp) = base.checked_add(Duration::from_secs(add_secs)) else {
        return Some(Duration::MAX);
    };
    exp.duration_since(now).ok()
}

/// Parse AWS SigV4 `YYYYMMDDTHHMMSSZ` UTC timestamp into `SystemTime`.
fn parse_amz_date(raw: &str) -> Option<SystemTime> {
    let b = raw.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let year: i64 = raw.get(0..4)?.parse().ok()?;
    let month: u32 = raw.get(4..6)?.parse().ok()?;
    let day: u32 = raw.get(6..8)?.parse().ok()?;
    let hour: u64 = raw.get(9..11)?.parse().ok()?;
    let min: u64 = raw.get(11..13)?.parse().ok()?;
    let sec: u64 = raw.get(13..15)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    // Howard Hinnant civil-from-days inverse algorithm.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + (doe as i64) - 719_468;
    if days < 0 {
        return None;
    }
    let total_secs = (days as u64) * 86_400 + hour * 3600 + min * 60 + sec;
    UNIX_EPOCH.checked_add(Duration::from_secs(total_secs))
}

/// Process-wide resolution used for every remote ffprobe/ffmpeg input.
pub fn resolve_remote(url: &str) -> String {
    REDIRECTS.resolve(url)
}

/// Drop a process-wide cached target (e.g. after remote media read fails).
pub fn invalidate_remote(url: &str) {
    REDIRECTS.invalidate(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control_directives_limit_or_disable_caching() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            response_cache_ttl(DEFAULT_TTL, Some("no-store"), None, None, None, now),
            None
        );
        assert_eq!(
            response_cache_ttl(
                DEFAULT_TTL,
                Some("private, no-cache, max-age=120"),
                None,
                None,
                None,
                now
            ),
            None
        );
        assert_eq!(
            response_cache_ttl(DEFAULT_TTL, Some("max-age=0"), None, None, None, now),
            None
        );
        assert_eq!(
            response_cache_ttl(
                DEFAULT_TTL,
                Some("public, max-age=45"),
                Some("Wed, 21 Oct 2099 07:28:00 GMT"),
                None,
                None,
                now
            ),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            response_cache_ttl(DEFAULT_TTL, Some("max-age=3600"), None, None, None, now),
            Some(DEFAULT_TTL)
        );
    }

    #[test]
    fn expires_header_and_signed_url_expiry_bound_ttl() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            response_cache_ttl(DEFAULT_TTL, None, Some("0"), None, None, now),
            None
        );
        let date_str = httpdate::fmt_http_date(now);
        let exp_str = httpdate::fmt_http_date(now + Duration::from_secs(20));
        assert_eq!(
            response_cache_ttl(
                DEFAULT_TTL,
                None,
                Some(&exp_str),
                Some(&date_str),
                None,
                now
            ),
            Some(Duration::from_secs(20))
        );
        let signed = format!(
            "https://cdn.example/video.mkv?Expires={}",
            1_700_000_000 + 30
        );
        assert_eq!(
            response_cache_ttl(DEFAULT_TTL, None, None, None, Some(&signed), now),
            Some(Duration::from_secs(25))
        );
        let nearly_expired = format!(
            "https://cdn.example/video.mkv?Expires={}",
            1_700_000_000 + 3
        );
        assert_eq!(
            response_cache_ttl(DEFAULT_TTL, None, None, None, Some(&nearly_expired), now),
            None
        );
    }
}
