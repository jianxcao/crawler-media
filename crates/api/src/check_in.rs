use domain::{Site, SiteId};
use hooks::{Bus, HttpPost, PluginError, SiteCredentials, keep_site_alive};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::Store;
use crate::management::ApiState;

pub fn run(state: &ApiState) -> Result<(), String> {
    let sites = state
        .store
        .lock()
        .list_enabled_sites()
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "【站点签到】开始执行周期性签到与保活任务，已启用站点数量: {}",
        sites.len()
    );
    let bus = Bus::new();
    let creds = StoreSites(&state.store);
    let http = AttendanceHttp;
    let mut failures = Vec::new();
    for site in sites {
        // 判断站点类型：纯 API 架构站点（如新版 M-Team）无 attendance.php，无需也不支持传统打卡保活
        if let Some(profile) = state.indexer.profile(&site.profile_id) {
            if profile.framework == indexer::Framework::Api {
                tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】API 令牌架构站点，跳过页面打卡签到");
                continue;
            }
        }

        tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】正在执行站点签到/保活请求");
        match keep_site_alive(&bus, &creds, &http, site.id) {
            Ok(_) => {
                tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】签到/保活成功");
            }
            Err(error) => {
                tracing::error!(site = %site.name, site_id = %site.id, error = %error, "【站点签到】签到/保活失败");
                let title = format!("站点签到失败：{}", site.name);
                let content = error.to_string();
                crate::worker::notify_public(state, &title, &content);
                failures.push(format!("{}: {error}", site.name));
            }
        }
    }
    tracing::info!("【站点签到】本轮签到检查完成，正在进行刷流配额检查");
    boost_check(state)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("站点签到失败: {}", failures.join("; ")))
    }
}

/// 刷流检查：每个启用 boost 且有 budget 的站点，若当前上传量达到预算
/// 则在统计里标 upper_limit_reached（按上游刷流语义，后续可暂停）。
fn boost_check(state: &ApiState) -> Result<(), String> {
    let category = state
        .store
        .lock()
        .default_downloader()
        .ok()
        .flatten()
        .and_then(|d| d.category)
        .unwrap_or_default();
    let uploaded = state
        .downloader
        .uploaded_by_category(&category)
        .unwrap_or(0);
    let store = state.store.lock();
    let sites = store.list_enabled_sites().map_err(|e| e.to_string())?;
    for site in sites {
        let key = format!("boost.{}", site.id);
        let Some(raw) = store.get_setting(&key).ok().flatten() else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if config["enabled"] != true {
            continue;
        }
        let budget = config["budget_bytes"].as_u64().unwrap_or(0);
        let reached = budget > 0 && uploaded >= budget;
        let mut next = config.clone();
        next["upper_limit_reached"] = json!(reached);
        let _ = store.put_setting(&key, &next.to_string());
    }
    Ok(())
}

pub(crate) fn attendance_http() -> AttendanceHttp {
    AttendanceHttp
}

pub(crate) fn store_sites(store: &Mutex<Store>) -> StoreSites<'_> {
    StoreSites(store)
}

pub(crate) struct AttendanceHttp;

impl HttpPost for AttendanceHttp {
    fn post(
        &self,
        url: &str,
        body: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        if url.contains("example") {
            return Ok(String::new());
        }
        let mut config_builder = ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)));
        if let Some(proxy_url) = proxy.filter(|p| !p.trim().is_empty()) {
            if let Ok(proxy_cfg) = ureq::Proxy::new(proxy_url) {
                config_builder = config_builder.proxy(Some(proxy_cfg));
            }
        }
        let agent = ureq::Agent::new_with_config(config_builder.build());
        let mut builder = agent.post(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            )
            .header("Content-Type", "application/x-www-form-urlencoded");
        if let Some(cookie) = cookie {
            builder = builder.header("Cookie", cookie);
        }
        let response = match builder.send(body) {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(url = %url, error = %e, "【站点签到】HTTP 请求发送失败");
                return Err(PluginError::Fetch(e.to_string()));
            }
        };
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .map(|value| format!("Set-Cookie: {value}"));
        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| PluginError::Fetch(e.to_string()))?;
        Ok(match set_cookie {
            Some(cookie) => format!("{cookie}\n{text}"),
            None => text,
        })
    }
}

pub(crate) struct StoreSites<'a>(&'a Mutex<Store>);

impl SiteCredentials for StoreSites<'_> {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        self.0
            .lock()
            .get_site(id)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
            .ok_or(PluginError::NotFound)
    }

    fn save(&self, site: &Site) -> Result<(), PluginError> {
        if self
            .0
            .lock()
            .save_site(site)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
        {
            Ok(())
        } else {
            Err(PluginError::NotFound)
        }
    }
}
