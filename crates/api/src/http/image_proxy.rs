//! Proxy remote catalog posters so the browser is not blocked by Referer.
//! Fetched bytes are cached on disk under `{CRAWLER_MEDIA_DATA}/cache/images`
//! so a second visit never re-fetches the upstream image.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

mod cache;
mod runtime;

const IMAGE_RESPONSE_LIMIT: u64 = 8 * 1024 * 1024;
const IMAGE_CONCURRENCY: usize = 8;

use axum::extract::Query;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use url::Url;

#[derive(Deserialize)]
pub(crate) struct ProxyQuery {
    url: String,
}

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// 公开图片代理的磁盘预算。超限时丢掉最旧的缓存，避免任意 URL 把数据目录写满。
const IMAGE_CACHE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// 图床白名单：**解析后的主机名**精确匹配（或该域的子域）。
/// 不用 URL 子串判断——`http://127.0.0.1:9999/private?x=bgm.tv/` 这类
/// 夹带允许串的地址会绕过子串检查打到内网。
const ALLOWED_HOSTS: &[&str] = &[
    "doubanio.com",
    "douban.com",
    "image.tmdb.org",
    "thetvdb.com",
    "bgm.tv",
    "lain.bgm.tv",
    "anilist.co",
    "s4.anilist.co",
];

pub(crate) async fn proxy_image(headers: HeaderMap, query: Query<ProxyQuery>) -> Response {
    static PROXY: OnceLock<Arc<runtime::ImageProxy>> = OnceLock::new();
    PROXY
        .get_or_init(|| {
            Arc::new(runtime::ImageProxy::new(
                image_cache(),
                Arc::new(tokio::sync::Semaphore::new(IMAGE_CONCURRENCY)),
                Arc::new(fetch_image),
            ))
        })
        .response(query.0, headers)
        .await
}

fn image_cache() -> Arc<cache::ImageCache> {
    static CACHE: OnceLock<Arc<cache::ImageCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            Arc::new(cache::ImageCache::new(
                crate::config::data_dir_from_env()
                    .join("cache")
                    .join("images"),
                IMAGE_CACHE_BUDGET_BYTES,
                IMAGE_RESPONSE_LIMIT,
            ))
        })
        .clone()
}

#[cfg(test)]
async fn proxy_with_gate(query: ProxyQuery, gate: Arc<tokio::sync::Semaphore>) -> Response {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let proxy = Arc::new(runtime::ImageProxy::new(
        Arc::new(cache::ImageCache::new(dir.path().into(), 1000, 100)),
        gate.clone(),
        Arc::new(|_| Err("offline image fixture".into())),
    ));
    let response = proxy.response(query, HeaderMap::new()).await;
    gate.close();
    response
}

/// SipHash hex of the URL: a disk-cache filename, not a security primitive.
fn cache_key(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 白名单校验：解析 URL，取**主机名**（不是子串）做精确/子域匹配；
/// 拒绝 IP 字面量与 localhost（SSRF 到内网地址的通道）。
fn allowed_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // Host 为 IP 字面量（IPv4/IPv6）或 localhost → 一律拒绝。
    if parsed
        .host()
        .is_some_and(|host| !matches!(host, url::Host::Domain(_)))
        || host.eq_ignore_ascii_case("localhost")
    {
        return false;
    }
    ALLOWED_HOSTS.iter().any(|allowed| {
        host.eq_ignore_ascii_case(allowed)
            || host.to_ascii_lowercase().ends_with(&format!(".{allowed}"))
    })
}

/// 单跳、不自动跟随重定向的 agent（重定向由调用方逐跳校验后手动跟进）。
fn no_redirect_agent(url: &str) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_resolve(Some(Duration::from_secs(4)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(8)));

    let is_douban = url.contains("douban");
    let bypass_douban = is_douban && crate::http_agent::douban_bypass();

    if !bypass_douban {
        let proxy_opt = crate::http_agent::current_proxy().or_else(|| {
            std::env::var("HTTP_PROXY")
                .or_else(|_| std::env::var("http_proxy"))
                .or_else(|_| std::env::var("ALL_PROXY"))
                .or_else(|_| std::env::var("all_proxy"))
                .ok()
        });
        if let Some(proxy_url) = proxy_opt {
            let trimmed = proxy_url.trim();
            let remote_dns_url = if let Some(stripped) = trimmed.strip_prefix("socks5://") {
                format!("socks5h://{stripped}")
            } else {
                trimmed.to_string()
            };
            if let Ok(proxy) = ureq::Proxy::new(&remote_dns_url) {
                builder = builder.proxy(Some(proxy));
            } else if let Ok(proxy) = ureq::Proxy::new(trimmed) {
                builder = builder.proxy(Some(proxy));
            }
        }
    }

    builder.build().new_agent()
}

fn fetch_image(url: &str) -> Result<(String, Vec<u8>), String> {
    let agent = no_redirect_agent(url);
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut current = url.to_string();
    // 手动跟随重定向：每一跳都重新过白名单，最多 5 跳。
    for _ in 0..5 {
        if !allowed_url(&current) {
            return Err(format!("不允许的主机: {current}"));
        }
        let referer = if current.contains("douban") {
            "https://movie.douban.com/"
        } else if current.contains("bgm.tv") {
            "https://bgm.tv/"
        } else {
            ""
        };
        let mut request = agent.get(&current).header("User-Agent", UA);
        if !referer.is_empty() {
            request = request.header("Referer", referer);
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(|| "Image fetch deadline elapsed".to_string())?;
        let response = request
            .config()
            .timeout_global(Some(remaining))
            .build()
            .call()
            .map_err(|err| err.to_string())?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| format!("重定向 {status} 无 Location: {current}"))?;
            let base = Url::parse(&current).map_err(|e| e.to_string())?;
            current = base.join(&location).map_err(|e| e.to_string())?.to_string();
            continue;
        }
        if !status.is_success() {
            return Err(format!("上游返回 {status}: {current}"));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        if !content_type.starts_with("image/") {
            return Err(format!("上游不是图片: {content_type}"));
        }
        let bytes = read_image_body(response.into_body(), IMAGE_RESPONSE_LIMIT)?;
        return Ok((content_type, bytes));
    }
    Err("重定向过多".into())
}

fn read_image_body(body: ureq::Body, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    // ureq's body limit caps wire bytes before decompression. Also cap decoded bytes,
    // so a small compressed response cannot allocate an unbounded image buffer.
    let mut reader = body
        .into_with_config()
        .limit(limit + 1)
        .reader()
        .take(limit + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("Image response exceeds {limit} bytes"));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "image_proxy/tests.rs"]
mod cache_tests;

#[cfg(test)]
mod tests {
    use super::allowed_url;

    #[test]
    fn allows_whitelisted_image_hosts() {
        assert!(allowed_url("https://image.tmdb.org/t/p/w342/abc.jpg"));
        assert!(allowed_url(
            "https://img1.doubanio.com/view/photo/l/public/abc.jpg"
        ));
        assert!(allowed_url("https://lain.bgm.tv/pic/cover/l/abc.jpg"));
        assert!(allowed_url("https://artworks.thetvdb.com/banners/abc.jpg"));
        assert!(allowed_url("https://s4.anilist.co/file/anilistcdn/abc.jpg"));
    }

    #[test]
    fn rejects_ssrf_targets_and_host_spoofing() {
        // 夹带白名单子串的内网地址（报告里的复现）→ 拒绝。
        assert!(!allowed_url("http://127.0.0.1:9999/private?x=bgm.tv/"));
        assert!(!allowed_url(
            "http://169.254.169.254/latest/meta-data?y=thetvdb.com/"
        ));
        assert!(!allowed_url("http://[::1]:8080/x"));
        assert!(!allowed_url("http://localhost/x?z=anilist.co/"));
        // 形似白名单的域名 → 拒绝（子串匹配时代会放过）。
        assert!(!allowed_url("https://bgm.tv.evil.com/x"));
        assert!(!allowed_url("https://evilbgm.tv/x"));
        assert!(!allowed_url("https://notthetvdb.com/x"));
        assert!(!allowed_url("https://image.tmdb.org.evil.net/x"));
        // 非 http(s) 协议。
        assert!(!allowed_url("ftp://image.tmdb.org/x"));
        // 无主机。
        assert!(!allowed_url("https:///path"));
    }
}
