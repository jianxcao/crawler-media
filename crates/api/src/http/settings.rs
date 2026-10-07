//! Settings: metadata source credentials (TMDB etc.) stored in settings KV.
//! Keys are read live by the catalog clients, so saving here applies at once.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::settings_keys;

fn masked(key: Option<String>) -> Value {
    match key.filter(|k| !k.is_empty()) {
        Some(k) if k.len() > 8 => json!(format!("{}…{}", &k[..4], &k[k.len() - 4..])),
        Some(_) => json!("已配置"),
        None => Value::Null,
    }
}

/// Metadata source configuration + status.
pub(crate) async fn get_metadata(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let tmdb = store
        .get_setting(settings_keys::TMDB_API_KEY)
        .ok()
        .flatten();
    let tvdb = store
        .get_setting(settings_keys::TVDB_API_KEY)
        .ok()
        .flatten();
    let language = store
        .get_setting(settings_keys::METADATA_LANGUAGE)
        .ok()
        .flatten();
    ok(json!({
        "language": language.unwrap_or_default(),
        "tmdb": {
            "configured": tmdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tmdb.clone()),
            "env_override": crate::config::tmdb_key_from_env_is_set(),
        },
        "tvdb": {
            "configured": tvdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tvdb.clone()),
        },
        "sources": [
            { "id": "tmdb", "name": "TMDB", "needs_key": true, "configured": tmdb.as_deref().is_some_and(|k| !k.is_empty()) },
            { "id": "douban", "name": "豆瓣", "needs_key": false, "configured": true },
            { "id": "tvdb", "name": "TVDB", "needs_key": true, "configured": tvdb.as_deref().is_some_and(|k| !k.is_empty()) },
            { "id": "bangumi", "name": "Bangumi", "needs_key": false, "configured": true },
            { "id": "anilist", "name": "AniList", "needs_key": false, "configured": true },
        ],
    }))
    .into_response()
}

/// Save metadata source keys (empty string clears).
pub(crate) async fn put_metadata(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let (tmdb, tvdb) = {
        let store = state.store.lock();
        if let Some(key) = body["tmdb_api_key"].as_str() {
            if let Err(error) = store.put_setting(settings_keys::TMDB_API_KEY, key) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        if let Some(key) = body["tvdb_api_key"].as_str() {
            if let Err(error) = store.put_setting(settings_keys::TVDB_API_KEY, key) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        if let Some(language) = body["language"].as_str() {
            if let Err(error) = store.put_setting(settings_keys::METADATA_LANGUAGE, language) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        (
            store
                .get_setting(settings_keys::TMDB_API_KEY)
                .ok()
                .flatten(),
            store
                .get_setting(settings_keys::TVDB_API_KEY)
                .ok()
                .flatten(),
        )
    };
    ok(json!({
        "tmdb": {
            "configured": tmdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tmdb),
        },
        "tvdb": {
            "configured": tvdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tvdb),
        },
        "restart_required": true,
        "message": "元数据配置已保存，TVDB及全局语言变更将在服务重启后完整生效",
    }))
    .into_response()
}

/// Test the configured TMDB key with a live search.
pub(crate) async fn test_metadata(State(state): State<ApiState>) -> Response {
    let api_key = {
        let store = state.store.lock();
        store
            .get_setting(settings_keys::TMDB_API_KEY)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())
    };
    let Some(api_key) = api_key else {
        return err(
            StatusCode::BAD_REQUEST,
            "metadata.unconfigured",
            "请先填写 TMDB API Key",
        );
    };
    // G06: 绕过本地 catalog 缓存，直连 TMDB 验证当前配置的 key 是否真实有效
    let test_url =
        format!("https://api.themoviedb.org/3/search/movie?query=Dune&api_key={api_key}");
    let res = tokio::task::spawn_blocking(move || {
        crate::http_agent::call(|agent| agent.get(&test_url).call())
    })
    .await;

    match res {
        Ok(Ok(resp)) => {
            let status = resp.status().as_u16();
            if status >= 400 {
                return ok(json!({
                    "ok": false,
                    "error": format!("TMDB API 响应错误 (HTTP {status})，请检查 API Key")
                }))
                .into_response();
            }
            let body_str = match resp.into_body().read_to_string() {
                Ok(s) => s,
                Err(e) => {
                    return ok(json!({
                        "ok": false,
                        "error": format!("读取响应失败: {e}")
                    }))
                    .into_response();
                }
            };
            let parsed: serde_json::Value = match serde_json::from_str(&body_str) {
                Ok(v) => v,
                Err(e) => {
                    return ok(json!({
                        "ok": false,
                        "error": format!("解析 TMDB 响应失败: {e}")
                    }))
                    .into_response();
                }
            };
            let results = parsed.get("results").and_then(|r| r.as_array());
            let count = results.map(|r| r.len()).unwrap_or(0);
            let sample = results
                .and_then(|r| r.first())
                .and_then(|item| item.get("title").or_else(|| item.get("name")))
                .and_then(|t| t.as_str())
                .unwrap_or("Dune")
                .to_string();
            ok(json!({
                "ok": true,
                "result_count": count,
                "sample": sample,
            }))
            .into_response()
        }
        Ok(Err(err)) => ok(json!({
            "ok": false,
            "error": format!("请求 TMDB 失败: {err}")
        }))
        .into_response(),
        Err(err) => ok(json!({
            "ok": false,
            "error": format!("执行测试任务失败: {err}")
        }))
        .into_response(),
    }
}

pub(crate) use super::proxy_settings::{diagnose_proxy, get_proxy, put_proxy, test_proxy};

/// Get CDP and Obscura browser settings.
pub(crate) async fn get_browser_settings(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let cdp_enabled = store
        .get_setting(settings_keys::CDP_SYNC_ENABLED)
        .ok()
        .flatten()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let cdp_url = store
        .get_setting(settings_keys::CDP_URL)
        .ok()
        .flatten()
        .unwrap_or_else(|| "http://127.0.0.1:9222".into());

    let obscura_enabled = store
        .get_setting(settings_keys::OBSCURA_ENABLED)
        .ok()
        .flatten()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let obscura_url = store
        .get_setting(settings_keys::OBSCURA_URL)
        .ok()
        .flatten()
        .unwrap_or_else(|| "http://127.0.0.1:9223".into());

    let obscura_installed = state.obscura.is_installed();
    let obscura_running = state.obscura.is_running();
    let obscura_downloading = state.obscura.is_downloading();

    let global_ua = crate::user_agent::get_user_agent(&store);

    ok(json!({
        "cdp": {
            "enabled": cdp_enabled,
            "url": cdp_url,
        },
        "obscura": {
            "enabled": obscura_enabled,
            "url": obscura_url,
            "installed": obscura_installed,
            "running": obscura_running,
            "downloading": obscura_downloading,
        },
        "user_agent": global_ua,
    }))
    .into_response()
}

/// Save CDP and Obscura settings.
pub(crate) async fn put_browser_settings(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let (cdp_enabled, cdp_url, obscura_enabled, obscura_url) = {
        let store = state.store.lock();
        if let Some(cdp) = body.get("cdp") {
            if let Some(enabled) = cdp.get("enabled").and_then(|v| v.as_bool()) {
                let _ = store.put_setting(
                    settings_keys::CDP_SYNC_ENABLED,
                    if enabled { "1" } else { "0" },
                );
            }
            if let Some(url) = cdp.get("url").and_then(|v| v.as_str()) {
                let _ = store.put_setting(settings_keys::CDP_URL, url);
            }
        }
        if let Some(obscura) = body.get("obscura") {
            if let Some(enabled) = obscura.get("enabled").and_then(|v| v.as_bool()) {
                let _ = store.put_setting(
                    settings_keys::OBSCURA_ENABLED,
                    if enabled { "1" } else { "0" },
                );
            }
            if let Some(url) = obscura.get("url").and_then(|v| v.as_str()) {
                let _ = store.put_setting(settings_keys::OBSCURA_URL, url);
            }
        }
        if let Some(ua) = body.get("user_agent").and_then(|v| v.as_str()) {
            let _ = store.put_setting(settings_keys::GLOBAL_USER_AGENT, ua.trim());
            crate::user_agent::sync_user_agent(&store);
        }
        let ce = store
            .get_setting(settings_keys::CDP_SYNC_ENABLED)
            .ok()
            .flatten()
            .map(|v| v == "1")
            .unwrap_or(false);
        let cu = store
            .get_setting(settings_keys::CDP_URL)
            .ok()
            .flatten()
            .unwrap_or_else(|| "http://127.0.0.1:9222".into());
        let oe = store
            .get_setting(settings_keys::OBSCURA_ENABLED)
            .ok()
            .flatten()
            .map(|v| v == "1")
            .unwrap_or(false);
        let ou = store
            .get_setting(settings_keys::OBSCURA_URL)
            .ok()
            .flatten()
            .unwrap_or_else(|| "http://127.0.0.1:9223".into());
        (ce, cu, oe, ou)
    };

    // 如果开启了 Obscura，后台自动启动进程（若未安装则自动下载安装）
    if obscura_enabled {
        let obscura = state.obscura.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(err) = obscura.start() {
                tracing::error!(error = %err, "【Obscura】自动拉起失败");
            }
        });
    } else {
        // 关闭时自动停止
        let obscura = state.obscura.clone();
        tokio::task::spawn_blocking(move || {
            obscura.stop();
        });
    }

    let obscura_installed = state.obscura.is_installed();
    let obscura_running = state.obscura.is_running();
    let obscura_downloading = state.obscura.is_downloading();

    let global_ua = {
        let store = state.store.lock();
        crate::user_agent::get_user_agent(&store)
    };

    ok(json!({
        "cdp": {
            "enabled": cdp_enabled,
            "url": cdp_url,
        },
        "obscura": {
            "enabled": obscura_enabled,
            "url": obscura_url,
            "installed": obscura_installed,
            "running": obscura_running,
            "downloading": obscura_downloading,
        },
        "user_agent": global_ua,
    }))
    .into_response()
}

/// Trigger CDP Cookie Sync across all supported PT sites.
/// Discovers cookies from Chrome, updates existing sites, and returns discovered candidate sites.
pub(crate) async fn sync_cdp_cookies(State(state): State<ApiState>) -> Response {
    let (cdp_enabled, cdp_url) = {
        let store = state.store.lock();
        let enabled = store
            .get_setting(settings_keys::CDP_SYNC_ENABLED)
            .ok()
            .flatten()
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        let url = store
            .get_setting(settings_keys::CDP_URL)
            .ok()
            .flatten()
            .unwrap_or_else(|| "http://127.0.0.1:9222".into());
        (enabled, url)
    };

    if !cdp_enabled {
        return err(
            StatusCode::BAD_REQUEST,
            "cdp.disabled",
            "CDP 同步未开启，请先在系统设置中启用 CDP 开关",
        );
    }

    // 1. Fetch raw cookies from Chrome CDP in a blocking thread
    let cookies_res =
        tokio::task::spawn_blocking(move || indexer::fetch_cookies_from_cdp(&cdp_url))
            .await
            .unwrap_or_else(|e| Err(indexer::IndexerError::Fetch(e.to_string())));

    let raw_cookies = match cookies_res {
        Ok(c) => c,
        Err(e) => return err(StatusCode::BAD_GATEWAY, "cdp.fetch_error", &e.to_string()),
    };

    // 2. Build list of all supported profile domains
    let profile_domains: Vec<(String, String)> = state
        .indexer
        .profile_ids()
        .into_iter()
        .filter_map(|pid| {
            let profile = state.indexer.profile(&pid)?;
            let domain = if let Some(base) = &profile.base_url {
                url_domain(base)
            } else if let Some(web_base) = &profile.web_base_url {
                url_domain(web_base)
            } else {
                url_domain(&profile.search.path)
            };
            domain.map(|d| (pid, d))
        })
        .collect();

    let matched = indexer::match_cookies_to_profiles(&raw_cookies, &profile_domains);

    // 3. Update existing sites in DB and find unconfigured candidates
    let store = state.store.lock();
    let existing_sites = store.list_sites().unwrap_or_default();
    let mut updated_count = 0;
    let mut candidate_sites = Vec::new();

    for item in &matched {
        if let Some(mut existing) = existing_sites
            .iter()
            .find(|s| s.profile_id == item.profile_id)
            .cloned()
        {
            // Update cookie
            existing.cookie = Some(item.cookie_header.clone());
            let _ = store.save_site(&existing);
            updated_count += 1;
        } else {
            // Candidate site that user has logged in on Chrome but not yet added
            candidate_sites.push(json!({
                "profile_id": item.profile_id,
                "domain": item.domain,
                "cookie_count": item.cookie_count,
            }));
        }
    }

    ok(json!({
        "total_cookies_scanned": raw_cookies.len(),
        "matched_sites": matched.len(),
        "updated_sites": updated_count,
        "candidate_sites": candidate_sites,
    }))
    .into_response()
}

fn url_domain(raw: &str) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(raw) {
        return parsed
            .host_str()
            .map(|h| h.trim_start_matches('.').to_string());
    }
    // Fallback if raw is like "pterclub.net"
    if raw.contains('.') && !raw.starts_with('/') {
        return Some(raw.trim_start_matches('.').to_string());
    }
    None
}
