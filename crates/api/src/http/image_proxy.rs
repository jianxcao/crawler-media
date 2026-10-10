//! Proxy remote catalog posters so the browser is not blocked by Referer.
//! Fetched bytes are cached on disk under `{CRAWLER_MEDIA_DATA}/cache/images`
//! so a second visit never re-fetches the upstream image.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::Query;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use url::Url;

use crate::http::err;

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

pub(crate) async fn proxy_image(Query(query): Query<ProxyQuery>) -> Response {
    if !allowed_url(&query.url) {
        return err(StatusCode::BAD_REQUEST, "image.invalid", "不允许的图床");
    }
    match tokio::task::spawn_blocking(move || cached_fetch(&query.url)).await {
        Ok(Ok((content_type, bytes))) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "public, max-age=86400".into()),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(error)) => err(StatusCode::BAD_GATEWAY, "image.fetch", &error),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "image.fetch",
            &error.to_string(),
        ),
    }
}

fn cache_root() -> PathBuf {
    crate::config::data_dir_from_env()
        .join("cache")
        .join("images")
}

fn cached_fetch(url: &str) -> Result<(String, Vec<u8>), String> {
    let key = cache_key(url);
    let root = cache_root();
    let cache_file = root.join(format!("{key}.img"));
    if let Ok(bytes) = std::fs::read(&cache_file) {
        if let Some((content_type, _)) = split_meta(&bytes) {
            return Ok((content_type, strip_meta(bytes)));
        }
    }
    let (content_type, bytes) = fetch_image(url)?;
    // Best-effort write: cache miss must not fail the request.
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!("[image_proxy] cache dir: {error}");
    } else {
        let mut meta = content_type.clone().into_bytes();
        meta.push(b'\n');
        let mut payload = meta;
        payload.extend_from_slice(&bytes);
        if let Err(error) =
            std::fs::File::create(&cache_file).and_then(|mut file| file.write_all(&payload))
        {
            eprintln!("[image_proxy] cache write: {error}");
        } else if let Err(error) = trim_image_cache(&root, IMAGE_CACHE_BUDGET_BYTES) {
            eprintln!("[image_proxy] cache trim: {error}");
        }
    }
    Ok((content_type, bytes))
}

/// SipHash hex of the URL: fine for a disk-cache filename (not security).
fn trim_image_cache(root: &std::path::Path, budget: u64) -> std::io::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            files.push((
                meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                meta.len(),
                entry.path(),
            ));
        }
    }
    let mut total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= budget {
        return Ok(());
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in files {
        if total <= budget {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
    Ok(())
}

fn cache_key(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Cache layout: `content-type\n` followed by image bytes.
fn split_meta(payload: &[u8]) -> Option<(String, &[u8])> {
    let split = payload.iter().position(|byte| *byte == b'\n')?;
    let content_type = String::from_utf8(payload[..split].to_vec()).ok()?;
    Some((content_type, &payload[split + 1..]))
}

fn strip_meta(payload: Vec<u8>) -> Vec<u8> {
    match payload.iter().position(|byte| *byte == b'\n') {
        Some(split) => payload[split + 1..].to_vec(),
        None => payload,
    }
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
            let remote_dns_url = if trimmed.starts_with("socks5://") {
                format!("socks5h://{}", &trimmed["socks5://".len()..])
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
        let response = request.call().map_err(|err| err.to_string())?;
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
        let bytes = response
            .into_body()
            .read_to_vec()
            .map_err(|err| err.to_string())?;
        return Ok((content_type, bytes));
    }
    Err("重定向过多".into())
}

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
