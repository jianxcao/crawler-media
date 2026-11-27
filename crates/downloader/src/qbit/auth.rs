use super::*;

pub(super) fn trimmed_api_key(config: &QbitConfig) -> Option<&str> {
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

impl QbitDownloader {
    pub fn connect(config: QbitConfig) -> Result<Self, DownloaderError> {
        let cookie = if let Some(key) = trimmed_api_key(&config) {
            Self::verify_api_key(&config, key)?;
            tracing::info!(url = %config.url, auth = "api_key", "qBittorrent 连接成功");
            String::new()
        } else {
            let cookie = Self::login(&config)?;
            tracing::info!(url = %config.url, user = %config.username, auth = "password", "qBittorrent 连接成功");
            cookie
        };
        Ok(Self {
            config,
            cookie: parking_lot::Mutex::new(cookie),
            login_lock: parking_lot::Mutex::new(()),
        })
    }

    fn verify_api_key(config: &QbitConfig, api_key: &str) -> Result<(), DownloaderError> {
        let version_url = join(&config.url, "/api/v2/app/version");
        let bearer = format!("Bearer {api_key}");
        tracing::debug!(url = %version_url, "正在使用 API Key 验证 qBittorrent");
        let response = agent_for_url(&config.url, true)
            .get(&version_url)
            .header("Authorization", &bearer)
            .call()
            .map_err(|e| {
                tracing::error!(url = %config.url, error = %e, "qBittorrent API Key 验证失败");
                match e {
                    ureq::Error::StatusCode(code @ (401 | 403)) => DownloaderError::Message(format!(
                        "qBittorrent API Key 认证失败（HTTP {code}），请检查 API Key 是否有效（需 qBittorrent v5.2+）"
                    )),
                    other => http_err(other),
                }
            })?;
        let _ = read_body(response)?;
        Ok(())
    }

    fn login(config: &QbitConfig) -> Result<String, DownloaderError> {
        let body = format!(
            "username={}&password={}",
            form_enc(&config.username),
            form_enc(&config.password)
        );
        let login_url = join(&config.url, "/api/v2/auth/login");
        tracing::debug!(url = %login_url, user = %config.username, "正在连接 qBittorrent");
        let response = agent_for_url(&config.url, true)
            .post(&login_url)
            .header("Referer", &config.url)
            .header("Origin", config.url.trim_end_matches('/'))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body)
            .map_err(|e| {
                tracing::error!(url = %config.url, error = %e, "连接 qBittorrent 失败");
                http_err(e)
            })?;
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|raw| raw.split(';').next())
            .unwrap_or("")
            .to_string();
        if cookie.is_empty() {
            tracing::error!(url = %config.url, "qBittorrent 登录未返回认证 Cookie");
            return Err(DownloaderError::Message(
                "qBittorrent login produced no cookie".into(),
            ));
        }
        Ok(cookie)
    }

    pub(super) fn cookie(&self) -> String {
        self.cookie.lock().clone()
    }

    /// Log in again unless another thread already refreshed the cookie
    /// since `stale` was used.
    pub(super) fn relogin(&self, stale: &str) -> Result<(), DownloaderError> {
        let _serial = self.login_lock.lock();
        if *self.cookie.lock() != stale {
            return Ok(());
        }
        tracing::warn!(url = %self.config.url, "qBittorrent 会话失效（403），重新登录");
        let fresh = Self::login(&self.config)?;
        *self.cookie.lock() = fresh;
        Ok(())
    }

    /// Run an authenticated request using either Bearer API key (v5.2+) or session cookie.
    /// Cookie mode re-logs in once on 403 (D-H1); API key mode never attempts cookie login.
    pub(super) fn authed<T>(
        &self,
        op: impl Fn(&str, &str) -> Result<T, ureq::Error>,
    ) -> Result<T, DownloaderError> {
        if let Some(key) = trimmed_api_key(&self.config) {
            let bearer = format!("Bearer {key}");
            return match op("Authorization", &bearer) {
                Err(ureq::Error::StatusCode(code @ (401 | 403))) => {
                    tracing::error!(url = %self.config.url, status = code, "qBittorrent API Key 认证失败");
                    Err(DownloaderError::Message(format!(
                        "qBittorrent API Key authentication failed ({code})"
                    )))
                }
                other => other.map_err(http_err),
            };
        }
        let cookie = self.cookie();
        match op("Cookie", &cookie) {
            Err(ureq::Error::StatusCode(403)) => {
                self.relogin(&cookie)?;
                match op("Cookie", &self.cookie()) {
                    Err(ureq::Error::StatusCode(403)) => {
                        tracing::error!(url = %self.config.url, "qBittorrent 重新登录后仍返回 403，认证失败");
                        Err(DownloaderError::Message(
                            "qBittorrent authentication failed (403 after re-login)".into(),
                        ))
                    }
                    other => other.map_err(http_err),
                }
            }
            other => other.map_err(http_err),
        }
    }
}
