use crate::{DownloaderId, SiteId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MaintenanceVia {
    #[default]
    Http,
    Browser,
}

impl MaintenanceVia {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Browser => "browser",
        }
    }
}

impl std::str::FromStr for MaintenanceVia {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "browser" => Ok(Self::Browser),
            other => Err(format!("unknown maintenance_via '{other}'")),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Site {
    pub id: SiteId,
    pub name: String,
    pub url: String,
    pub profile_id: String,
    pub cookie: Option<String>,
    pub api_key: Option<String>,
    pub rss_url: Option<String>,
    pub proxy: Option<String>,
    pub rate_limit_per_minute: Option<u32>,
    pub cdp_url: Option<String>,
    pub downloader_id: Option<DownloaderId>,
    pub enabled: bool,
    pub maintenance_via: MaintenanceVia,
}

/// 手写 `Debug`：`cookie` 是站点会话凭证、`api_key` 是站点 API 凭证，
/// 两者都绝不能进日志；`proxy` 可能内嵌 `user:pass@`，因此走全仓统一的
/// [`mask_proxy_url`]（保留用户名、掩码口令）。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?site)` 都会把凭证明文打进
/// 日志与前端"系统日志"。`Site` 本身没有 serde 派生（UI 载荷由 api 层单独
/// 构造），因此这里只影响 `Debug`。其余字段保留原样，便于诊断。
impl std::fmt::Debug for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Site")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("profile_id", &self.profile_id)
            .field("cookie", &self.cookie.as_ref().map(|_| "<redacted>"))
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("rss_url", &self.rss_url)
            .field("proxy", &self.proxy.as_deref().map(mask_proxy_url))
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .field("cdp_url", &self.cdp_url)
            .field("downloader_id", &self.downloader_id)
            .field("enabled", &self.enabled)
            .field("maintenance_via", &self.maintenance_via)
            .finish()
    }
}

/// 代理 URL 脱敏渲染的**全仓唯一实现**。
///
/// `scheme://user:pass@host:port` → `scheme://user:***@host:port`：**保留用户名**
/// （排障时"哪个账号配错了"是有用信息）、只掩码口令，因为口令才是凭证。这与
/// api 侧既有调用点/断言（`ServerConfig.metadata_proxy`、`ProxyEnv.url`、
/// `http_agent` 的日志行、`proxy_settings` 的 `active_proxy`）完全一致，因此
/// 统一后 api 行为不变。
///
/// 规则：
/// - 只有用户名、没有口令的 userinfo 原样保留（没有可隐藏的秘密）；
/// - 没有 userinfo 的 URL 原样返回；
/// - path / query / fragment 里的 `@` 不会被误当成 userinfo（先按
///   `/`、`?`、`#` 切出 authority 再找 `@`）；
/// - 没有 scheme 的 `user:pass@host:port` 同样脱敏（旧 api 实现遇到这种输入
///   会原样返回，等于把口令写进日志）。
///
/// 放在 `domain`（最底层、不依赖任何上层 crate）是为了让 api 与 domain 共用
/// 同一份渲染；`domain` 不能反向依赖 api 的脱敏函数（违反 AGENTS.md 分层）。
pub fn mask_proxy_url(raw: &str) -> String {
    let (prefix, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), raw),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    // host 不可能含 `@`，取最后一个 `@` 才能正确处理口令里未转义的 `@`。
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _password)) => format!("{prefix}{user}:***@{host}{tail}"),
            None => raw.to_string(),
        },
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod debug_tests {
    use super::{Site, SiteId, mask_proxy_url};

    fn site_with_proxy(proxy: Option<&str>) -> Site {
        Site {
            id: SiteId::new(),
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: proxy.map(str::to_string),
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: super::MaintenanceVia::Http,
        }
    }

    #[test]
    fn debug_never_prints_site_credentials() {
        let site = Site {
            id: SiteId::new(),
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=cookie-secret".into()),
            api_key: Some("site-api-key-secret".into()),
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: super::MaintenanceVia::Http,
        };
        let rendered = format!("{site:?}");
        assert!(
            !rendered.contains("cookie-secret"),
            "Debug 不得输出 cookie: {rendered}"
        );
        assert!(
            !rendered.contains("site-api-key-secret"),
            "Debug 不得输出 api_key: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("pt.example"), "{rendered}");
        assert!(rendered.contains("demo"), "{rendered}");
    }

    /// 统一语义后：用户名保留（便于排障），口令必须消失。
    #[test]
    fn debug_masks_site_proxy_password_but_keeps_username() {
        let site = site_with_proxy(Some("http://bob:proxy-password-secret@127.0.0.1:7890"));
        let rendered = format!("{site:?}");
        assert!(
            !rendered.contains("proxy-password-secret"),
            "Debug 不得输出 proxy 口令: {rendered}"
        );
        assert!(
            rendered.contains("bob:***@127.0.0.1:7890"),
            "统一语义是保留用户名 + 掩码口令: {rendered}"
        );
    }

    #[test]
    fn mask_proxy_url_keeps_username_and_masks_password() {
        assert_eq!(
            mask_proxy_url("http://bob:pw@127.0.0.1:7890"),
            "http://bob:***@127.0.0.1:7890"
        );
        assert_eq!(
            mask_proxy_url("bob:pw@127.0.0.1:7890"),
            "bob:***@127.0.0.1:7890",
            "无 scheme 的 user:pass@host 同样要脱敏（旧 api 实现会原样返回）"
        );
        assert_eq!(
            mask_proxy_url("http://bob@127.0.0.1:7890"),
            "http://bob@127.0.0.1:7890",
            "只有用户名、没有口令时没有可隐藏的秘密，原样保留"
        );
        assert_eq!(mask_proxy_url("bob@127.0.0.1:7890"), "bob@127.0.0.1:7890");
        assert_eq!(
            mask_proxy_url("socks5://127.0.0.1:1080"),
            "socks5://127.0.0.1:1080",
            "无 userinfo 时原样返回"
        );
        assert_eq!(
            mask_proxy_url("http://bob:pw@host:1/path@x"),
            "http://bob:***@host:1/path@x",
            "path 里的 @ 不是 userinfo"
        );
        assert_eq!(
            mask_proxy_url("http://host:1/path@x"),
            "http://host:1/path@x",
            "无 userinfo 但 path 含 @ 时不得误判"
        );
        assert_eq!(
            mask_proxy_url("http://bob:p@ss@host:1"),
            "http://bob:***@host:1",
            "口令里未转义的 @ 不能被当成 host 分隔符"
        );
        assert_eq!(mask_proxy_url(""), "");
    }
}
