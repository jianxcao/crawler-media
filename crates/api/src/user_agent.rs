use crate::Store;
use crate::settings_keys;
use parking_lot::RwLock;
use std::sync::LazyLock;

pub const DEFAULT_USER_AGENT: &str = "crawler-media/0.1.0";
static ACTIVE_USER_AGENT: LazyLock<RwLock<String>> =
    LazyLock::new(|| RwLock::new(DEFAULT_USER_AGENT.to_string()));

/// 获取系统全局 User-Agent。
pub fn get_user_agent(store: &Store) -> String {
    store
        .get_setting(settings_keys::GLOBAL_USER_AGENT)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string())
}

pub fn active_user_agent() -> String {
    ACTIVE_USER_AGENT.read().clone()
}

pub fn active_product_ua() -> String {
    active_user_agent()
}

/// 同步全局 UA 到内存缓存、底层 marker/probe 以及 http_agent。
pub fn sync_user_agent(store: &Store) {
    let ua = get_user_agent(store);
    *ACTIVE_USER_AGENT.write() = ua.clone();
    marker::set_custom_probe_ua(Some(ua));
    crate::http_agent::refresh_direct_agents();
}
