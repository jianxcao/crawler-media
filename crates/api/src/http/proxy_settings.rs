use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::settings_keys;

/// Mask the password in a proxy URL before returning it to the UI.
///
/// Delegates to [`domain::mask_proxy_url`] — the single implementation shared
/// with `http_agent`'s log lines and the `Debug` of `ServerConfig`/`ProxyEnv`/
/// `Site`. The previous local copy only masked URLs containing `://`, so a
/// schemeless `user:pass@host` was echoed verbatim by `GET`/`PUT
/// /settings/proxy` while `diagnose_proxy` already masked it.
pub(crate) fn mask_proxy_url(url: &str) -> String {
    domain::mask_proxy_url(url)
}

/// GET /settings/proxy — Read current metadata proxy configuration.
pub(crate) async fn get_proxy(State(state): State<ApiState>) -> Response {
    super::store_wait::store_read(&state, &[store::Db::App], move |store| {
        let proxy_url = store
            .get_setting(settings_keys::PROXY_METADATA)
            .ok()
            .flatten()
            .unwrap_or_default();
        let username = store
            .get_setting(settings_keys::PROXY_USERNAME)
            .ok()
            .flatten()
            .unwrap_or_default();
        let has_password = store
            .get_setting(settings_keys::PROXY_PASSWORD)
            .ok()
            .flatten()
            .is_some_and(|p| !p.is_empty());
        let douban_bypass = store
            .get_setting(settings_keys::PROXY_DOUBAN_BYPASS)
            .ok()
            .flatten()
            .and_then(|v| crate::config::parse_bool(&v))
            .unwrap_or(true);
        let allowed_domains = store
            .get_setting(settings_keys::PROXY_ALLOWED_DOMAINS)
            .ok()
            .flatten()
            .unwrap_or_default();
        // 非空才算 override，且与 http_agent 用同一组 env 变量（空串不生效）。
        let env_override = crate::config::proxy_env(&crate::config::ProcessEnv)
            .url
            .is_some();

        let active_masked = crate::http_agent::current_proxy().map(|p| mask_proxy_url(&p));

        ok(json!({
            "proxy_url": proxy_url,
            "username": username,
            "has_password": has_password,
            "douban_bypass": douban_bypass,
            "allowed_domains": allowed_domains,
            "active_proxy": active_masked,
            "env_override": env_override,
        }))
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

/// PUT /settings/proxy — Save metadata proxy configuration and reload immediately.
///
/// Fields pinned by the environment are rejected with 409: the startup seed
/// would overwrite them on the next restart.
pub(crate) async fn put_proxy(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    for (field, key) in [
        ("proxy_url", settings_keys::PROXY_METADATA),
        ("username", settings_keys::PROXY_USERNAME),
        ("password", settings_keys::PROXY_PASSWORD),
    ] {
        if body[field].as_str().is_some()
            && let Some(message) = crate::config::env_pinned_message(key)
        {
            return err(StatusCode::CONFLICT, "settings.env_pinned", &message);
        }
    }
    super::store_wait::store_write(&state, move |store| {
        if let Some(proxy_url) = body["proxy_url"].as_str()
            && let Err(error) = store.put_setting(settings_keys::PROXY_METADATA, proxy_url.trim())
        {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        if let Some(username) = body["username"].as_str()
            && let Err(error) = store.put_setting(settings_keys::PROXY_USERNAME, username.trim())
        {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        if let Some(password) = body["password"].as_str()
            && let Err(error) = store.put_setting(settings_keys::PROXY_PASSWORD, password.trim())
        {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        if let Some(douban_bypass) = body["douban_bypass"].as_bool() {
            let val_str = if douban_bypass { "true" } else { "false" };
            if let Err(error) = store.put_setting(settings_keys::PROXY_DOUBAN_BYPASS, val_str) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        if let Some(allowed_domains) = body["allowed_domains"].as_str()
            && let Err(error) =
                store.put_setting(settings_keys::PROXY_ALLOWED_DOMAINS, allowed_domains.trim())
        {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        crate::http_agent::sync_from_store(&store);

        let proxy_url = store
            .get_setting(settings_keys::PROXY_METADATA)
            .ok()
            .flatten()
            .unwrap_or_default();
        let username = store
            .get_setting(settings_keys::PROXY_USERNAME)
            .ok()
            .flatten()
            .unwrap_or_default();
        let has_password = store
            .get_setting(settings_keys::PROXY_PASSWORD)
            .ok()
            .flatten()
            .is_some_and(|p| !p.is_empty());
        let douban_bypass = store
            .get_setting(settings_keys::PROXY_DOUBAN_BYPASS)
            .ok()
            .flatten()
            .and_then(|v| crate::config::parse_bool(&v))
            .unwrap_or(true);
        let allowed_domains = store
            .get_setting(settings_keys::PROXY_ALLOWED_DOMAINS)
            .ok()
            .flatten()
            .unwrap_or_default();
        let env_override = crate::config::proxy_env(&crate::config::ProcessEnv)
            .url
            .is_some();

        let active_masked = crate::http_agent::current_proxy().map(|p| mask_proxy_url(&p));

        ok(json!({
            "proxy_url": proxy_url,
            "username": username,
            "has_password": has_password,
            "douban_bypass": douban_bypass,
            "allowed_domains": allowed_domains,
            "env_override": env_override,
            "active_proxy": active_masked,
        }))
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error)
}

/// POST /settings/proxy/test — Probe connection through specified proxy.
pub(crate) async fn test_proxy(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let proxy_url = body["proxy_url"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let username = body["username"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = body["password"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let target = body["target"]
        .as_str()
        .unwrap_or("https://api.themoviedb.org");

    let formatted_proxy =
        proxy_url.map(|url| crate::http_agent::format_proxy_url(url, username, password));

    let started = std::time::Instant::now();
    let proxy_for_block = formatted_proxy;
    let target_for_block = target.to_string();

    let probe_res = state
        .http_work
        .run(super::library::BROWSE_DEADLINE, move || {
            crate::http_agent::call_with_proxy(proxy_for_block.as_deref(), |agent| {
                extract_http_status(
                    agent
                        .get(&target_for_block)
                        .header("User-Agent", "crawler-media-probe/1.0")
                        .call(),
                )
            })
        })
        .await;

    let elapsed_ms = started.elapsed().as_millis() as i64;
    let (probe_ok, error) = evaluate_probe_result(probe_res.map_err(|e| format!("调度异常: {e}")));

    ok(json!({
        "ok": probe_ok,
        "latency_ms": elapsed_ms,
        "error": error,
    }))
    .into_response()
}

static DIAGNOSE_GATE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();

fn diagnose_gate() -> std::sync::Arc<tokio::sync::Semaphore> {
    DIAGNOSE_GATE
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(5)))
        .clone()
}

/// POST /settings/proxy/diagnose — Check routing and connectivity for all major metadata domains.
pub(crate) async fn diagnose_proxy(State(state): State<ApiState>) -> Response {
    let active_proxy = crate::http_agent::current_proxy();
    let douban_bypass = crate::http_agent::douban_bypass();

    let targets = build_diagnose_targets();
    let mut set = tokio::task::JoinSet::new();

    for (idx, target) in targets.into_iter().enumerate() {
        let proxy = if target.bypassable_for_douban && douban_bypass {
            None
        } else {
            active_proxy.clone()
        };
        let is_proxied = proxy.is_some();
        let masked_proxy = proxy.as_deref().map(crate::http_agent::mask_proxy_url);

        let route_label = if let Some(p) = masked_proxy {
            format!("代理 ({p})")
        } else if target.bypassable_for_douban && douban_bypass && active_proxy.is_some() {
            "直连 (已绕过代理)".to_string()
        } else {
            "直连 (未配置代理)".to_string()
        };

        let budget = state.http_work.clone();
        set.spawn(async move {
            run_single_probe(idx, budget, target, proxy, is_proxied, route_label).await
        });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(val) = res {
            results.push(val);
        }
    }
    results.sort_by_key(|(idx, _)| *idx);
    let items: Vec<Value> = results.into_iter().map(|(_, val)| val).collect();

    ok(json!({
        "active_proxy": active_proxy.as_deref().map(crate::http_agent::mask_proxy_url),
        "douban_bypass": douban_bypass,
        "items": items,
    }))
    .into_response()
}

enum ProbeMethod {
    Get {
        ua: &'static str,
        referer: Option<&'static str>,
    },
    PostJson {
        ua: &'static str,
        body: &'static str,
    },
}

struct TargetProbe {
    name: &'static str,
    display_url: &'static str,
    probe_url: String,
    method: ProbeMethod,
    bypassable_for_douban: bool,
}

fn build_diagnose_targets() -> Vec<TargetProbe> {
    vec![
        TargetProbe {
            name: "TMDB (条目数据)",
            display_url: "https://api.themoviedb.org",
            probe_url: "https://api.themoviedb.org/3/configuration".to_string(),
            method: ProbeMethod::Get {
                ua: "crawler-media/0.1",
                referer: None,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "TMDB (海报封面 CDN)",
            display_url: "https://image.tmdb.org",
            probe_url: "https://image.tmdb.org/t/p/w92/7WsyChQLEftFiDOVTGkv3hFpyyt.jpg".to_string(),
            method: ProbeMethod::Get {
                ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
                referer: None,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "Bangumi (动漫番组)",
            display_url: "https://api.bgm.tv",
            probe_url: "https://api.bgm.tv/calendar".to_string(),
            method: ProbeMethod::Get {
                ua: "crawler-media/0.1 (https://github.com/jianxcao/crawler-media)",
                referer: None,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "AniList (海外番剧)",
            display_url: "https://graphql.anilist.co",
            probe_url: "https://graphql.anilist.co".to_string(),
            method: ProbeMethod::PostJson {
                ua: "crawler-media/0.1",
                body: r#"{"query":"{ Page(page: 1, perPage: 1) { media { id } } }"}"#,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "豆瓣电影 (华语条目)",
            display_url: "https://movie.douban.com",
            probe_url: "https://movie.douban.com".to_string(),
            method: ProbeMethod::Get {
                ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
                referer: Some("https://movie.douban.com/"),
            },
            bypassable_for_douban: true,
        },
    ]
}

async fn run_single_probe(
    idx: usize,
    budget: std::sync::Arc<crate::blocking_budget::BlockingBudget>,
    target: TargetProbe,
    proxy: Option<String>,
    is_proxied: bool,
    route_label: String,
) -> (usize, Value) {
    let started = std::time::Instant::now();
    let probe_url = target.probe_url.clone();
    let method = target.method;
    let proxy_for_block = proxy;

    let gate_permit = match tokio::time::timeout(
        super::library::BROWSE_DEADLINE,
        diagnose_gate().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err("调度异常: blocking capacity exhausted".to_string()),
        Err(_) => Err("调度异常: operation deadline elapsed".to_string()),
    };

    let probe_res = match gate_permit {
        Ok(gate_permit) => {
            let res = match budget.reserve(std::time::Duration::from_millis(80)).await {
                Ok(reservation) => {
                    let task = tokio::task::spawn_blocking(move || {
                        let _reservation = reservation;
                        crate::http_agent::call_with_proxy(proxy_for_block.as_deref(), |agent| {
                            let response = match method {
                                ProbeMethod::Get { ua, referer } => {
                                    let mut req = agent.get(&probe_url).header("User-Agent", ua);
                                    if let Some(ref_val) = referer {
                                        req = req.header("Referer", ref_val);
                                    }
                                    req.call()
                                }
                                ProbeMethod::PostJson { ua, body } => agent
                                    .post(&probe_url)
                                    .header("User-Agent", ua)
                                    .header("Content-Type", "application/json")
                                    .header("Accept", "application/json")
                                    .send(body),
                            };
                            extract_http_status(response)
                        })
                    });
                    match tokio::time::timeout(super::library::BROWSE_DEADLINE, task).await {
                        Ok(Ok(res)) => Ok(res),
                        Ok(Err(err)) => Err(format!("调度异常: blocking worker failed: {err}")),
                        Err(_) => Err("调度异常: operation deadline elapsed".to_string()),
                    }
                }
                Err(_) => Err("调度异常: blocking capacity exhausted".to_string()),
            };
            drop(gate_permit);
            res
        }
        Err(err) => Err(err),
    };

    let latency_ms = started.elapsed().as_millis() as i64;
    let (ok, error) = evaluate_probe_result(probe_res);

    (
        idx,
        json!({
            "name": target.name,
            "url": target.display_url,
            "is_proxied": is_proxied,
            "route_label": route_label,
            "ok": ok,
            "latency_ms": latency_ms,
            "error": error,
        }),
    )
}

fn extract_http_status(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<u16, ureq::Error> {
    match result {
        Ok(response) => Ok(response.status().as_u16()),
        Err(ureq::Error::StatusCode(code)) => Ok(code),
        Err(err) => Err(err),
    }
}

fn evaluate_probe_result(
    probe_res: Result<Result<u16, ureq::Error>, String>,
) -> (bool, Option<String>) {
    match probe_res {
        Ok(Ok(code)) | Ok(Err(ureq::Error::StatusCode(code))) => {
            if code < 500 {
                (true, None)
            } else {
                (false, Some(format!("HTTP 状态码: {code}")))
            }
        }
        Ok(Err(err)) => (false, Some(err.to_string())),
        Err(join_err) => (false, Some(join_err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_treats_ureq_status_401_and_404_as_network_reachable_without_error() {
        let extracted = extract_http_status(Err(ureq::Error::StatusCode(401)));
        assert_eq!(extracted.as_ref().ok().copied(), Some(401));
        let (ok, error) = evaluate_probe_result(Ok(extracted));
        assert!(ok, "401 from ureq should be treated as network reachable");
        assert_eq!(error, None);

        let (ok, error) =
            evaluate_probe_result(Ok(extract_http_status(Err(ureq::Error::StatusCode(404)))));
        assert!(ok, "404 from ureq should be treated as network reachable");
        assert_eq!(error, None);

        let (ok, error) =
            evaluate_probe_result(Ok(extract_http_status(Err(ureq::Error::StatusCode(502)))));
        assert!(!ok, "502 from ureq should be treated as failure");
        assert_eq!(error.as_deref(), Some("HTTP 状态码: 502"));
    }

    #[test]
    fn diagnose_targets_probe_tmdb_network_only_without_api_key() {
        let targets = build_diagnose_targets();
        let tmdb = targets
            .iter()
            .find(|t| t.display_url == "https://api.themoviedb.org")
            .expect("tmdb target exists");
        assert_eq!(tmdb.probe_url, "https://api.themoviedb.org/3/configuration");
    }
}
