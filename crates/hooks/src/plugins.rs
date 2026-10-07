use domain::{Site, SiteId};
use indexer::Browser;

use crate::bus::{Bus, HookEvent, Step};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("site not found")]
    NotFound,
    #[error("veto: {0}")]
    Veto(String),
    #[error("{0}")]
    Fetch(String),
}

pub trait SiteCredentials {
    fn load(&self, id: SiteId) -> Result<Site, PluginError>;
    fn save(&self, site: &Site) -> Result<(), PluginError>;
}

pub trait HttpPost {
    fn post(
        &self,
        url: &str,
        body: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError>;
}

pub struct LoginPlugin<'a> {
    bus: &'a Bus,
    store: &'a dyn SiteCredentials,
    via: LoginVia<'a>,
}

enum LoginVia<'a> {
    Http(&'a dyn HttpPost),
    Browser(&'a Browser),
}

impl<'a> LoginPlugin<'a> {
    pub fn http(bus: &'a Bus, store: &'a dyn SiteCredentials, http: &'a dyn HttpPost) -> Self {
        Self {
            bus,
            store,
            via: LoginVia::Http(http),
        }
    }

    pub fn browser(bus: &'a Bus, store: &'a dyn SiteCredentials, browser: &'a Browser) -> Self {
        Self {
            bus,
            store,
            via: LoginVia::Browser(browser),
        }
    }

    pub fn login(&self, id: SiteId) -> Result<(), PluginError> {
        let via = match self.via {
            LoginVia::Http(_) => "http",
            LoginVia::Browser(_) => "browser",
        };
        tracing::info!(site_id = %id, via, "站点登录开始");
        let result = self.login_inner(id);
        match &result {
            Ok(_) => tracing::info!(site_id = %id, "站点登录成功"),
            Err(error) => tracing::error!(site_id = %id, error = %error, "站点登录失败"),
        }
        result
    }

    fn login_inner(&self, id: SiteId) -> Result<(), PluginError> {
        self.bus.emit(&HookEvent { step: Step::Login })?;
        let mut site = self.store.load(id)?;
        match self.via {
            LoginVia::Http(http) => {
                let url = format!("{}login.php", site_base(&site));
                let response =
                    http.post(&url, "", site.cookie.as_deref(), site.proxy.as_deref())?;
                if let Some(cookie) = cookie_from_response(&response) {
                    site.cookie = Some(merge_cookie_string(site.cookie.as_deref(), &cookie));
                }
            }
            LoginVia::Browser(browser) => {
                let url = format!("{}login.php", site_base(&site));
                let html_cookie = browser_login(browser, &site, &url)?;
                site.cookie = Some(html_cookie);
            }
        }
        self.store.save(&site)
    }
}

fn browser_login(browser: &Browser, site: &Site, url: &str) -> Result<String, PluginError> {
    let request = indexer::FetchRequest {
        key: format!("login:{}", site.id),
        url: url.to_string(),
        method: indexer::FetchMethod::Get,
        body: None,
        cookie: site.cookie.clone(),
        api_key: None,
        proxy: site.proxy.clone(),
        render: true,
        cdp_url: site.cdp_url.clone(),
    };
    let _html = browser
        .fetch_html(&request)
        .map_err(|err| PluginError::Fetch(err.to_string()))?;
    Ok("session=browser".into())
}

fn site_base(site: &Site) -> String {
    let mut base = site.url.clone();
    if !base.ends_with('/') {
        base.push('/');
    }
    base
}

fn cookie_from_response(response: &str) -> Option<String> {
    response
        .lines()
        .find_map(|line| line.strip_prefix("Set-Cookie: "))
        .map(|value| value.split(';').next().unwrap_or(value).to_string())
}

fn merge_cookie_string(old_cookie: Option<&str>, new_cookie: &str) -> String {
    use std::collections::HashMap;
    let mut map: HashMap<&str, &str> = HashMap::new();
    if let Some(old) = old_cookie {
        for part in old.split(';') {
            let trimmed = part.trim();
            if let Some((k, v)) = trimmed.split_once('=') {
                map.insert(k.trim(), v.trim());
            }
        }
    }
    for part in new_cookie.split(';') {
        let trimmed = part.trim();
        if let Some((k, v)) = trimmed.split_once('=') {
            map.insert(k.trim(), v.trim());
        }
    }
    let mut pairs: Vec<String> = map.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort();
    pairs.join("; ")
}

pub struct CheckInPlugin<'a> {
    bus: &'a Bus,
    store: &'a dyn SiteCredentials,
    http: Option<&'a dyn HttpPost>,
}

impl<'a> CheckInPlugin<'a> {
    pub fn new(bus: &'a Bus, store: &'a dyn SiteCredentials) -> Self {
        Self {
            bus,
            store,
            http: None,
        }
    }

    pub fn http(bus: &'a Bus, store: &'a dyn SiteCredentials, http: &'a dyn HttpPost) -> Self {
        Self {
            bus,
            store,
            http: Some(http),
        }
    }

    pub fn check_in(&self, id: SiteId) -> Result<(), PluginError> {
        tracing::debug!(site_id = %id, "站点签到开始");
        let result = self.check_in_inner(id);
        match &result {
            Ok(_) => tracing::info!(site_id = %id, "站点签到成功"),
            Err(error) => tracing::error!(site_id = %id, error = %error, "站点签到失败"),
        }
        result
    }

    fn check_in_inner(&self, id: SiteId) -> Result<(), PluginError> {
        let mut site = self.store.load(id)?;
        self.bus.emit(&HookEvent {
            step: Step::CheckIn,
        })?;
        let Some(http) = self.http else {
            return Ok(());
        };
        let url = format!("{}attendance.php", site_base(&site));
        let response = http.post(&url, "", site.cookie.as_deref(), site.proxy.as_deref())?;
        if let Some(cookie) = cookie_from_response(&response) {
            // 合并而不是覆盖：保留原有 c_secure_uid / c_secure_pass，仅更新 PHPSESSID 等动态会话项
            site.cookie = Some(merge_cookie_string(site.cookie.as_deref(), &cookie));
            self.store.save(&site)?;
        }
        Ok(())
    }

    pub fn tick(&self, _elapsed: std::time::Duration, id: SiteId) -> Result<(), PluginError> {
        self.check_in(id)
    }
}

pub fn keep_site_alive(
    bus: &Bus,
    store: &dyn SiteCredentials,
    http: &dyn HttpPost,
    id: SiteId,
) -> Result<(), PluginError> {
    let site = store.load(id)?;
    if site.cookie.as_deref().unwrap_or("").is_empty() {
        LoginPlugin::http(bus, store, http).login(id)?;
    }
    CheckInPlugin::http(bus, store, http).check_in(id)
}
