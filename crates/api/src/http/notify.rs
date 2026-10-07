//! Notify client: config via settings KV; events post to jianxcao/notify.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

const NOTIFY_URL_KEY: &str = "notify_url";
const NOTIFY_TOKEN_KEY: &str = "notify_token";

pub fn notify_url(store: &crate::Store) -> Option<String> {
    store.get_setting(NOTIFY_URL_KEY).ok().flatten()
}

pub fn notify_token(store: &crate::Store) -> Option<String> {
    store.get_setting(NOTIFY_TOKEN_KEY).ok().flatten()
}

pub fn notify_configured(store: &crate::Store) -> bool {
    !notify_url(store).unwrap_or_default().is_empty()
        && !notify_token(store).unwrap_or_default().is_empty()
}

pub fn get_config_json(store: &crate::Store, is_admin: bool) -> Value {
    if is_admin {
        json!({
            "notify_url": notify_url(store).unwrap_or_default(),
            "notify_token": notify_token(store).unwrap_or_default(),
            "enabled": notify_configured(store),
        })
    } else {
        json!({
            "notify_url": notify_url(store).unwrap_or_default(),
            "notify_token": "",
            "enabled": notify_configured(store),
        })
    }
}

pub fn save_config(
    store: &crate::Store,
    url: &str,
    token: &str,
) -> Result<(), crate::store::StoreError> {
    store.put_setting(NOTIFY_URL_KEY, url)?;
    store.put_setting(NOTIFY_TOKEN_KEY, token)?;
    Ok(())
}

/// Post a message to the notify service. Errors are returned but callers
/// should treat them as best-effort (never block the loop).
pub fn send(store: &crate::Store, title: &str, content: &str) -> Result<(), String> {
    let base = notify_url(store).unwrap_or_default();
    let token = notify_token(store).unwrap_or_default();
    if base.is_empty() || token.is_empty() {
        return Ok(()); // not configured, skip silently
    }
    let app_id = base
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("system_alerts")
        .to_string();
    let root = base.trim_end_matches(&format!("/{}", app_id));
    let url = format!("{root}/api/v1/notify/{app_id}");
    let payload = json!({
        "title": title,
        "content": content,
    });
    let _ = ureq::post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(payload.to_string())
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// Send with explicit credentials (no store lock needed).
pub fn send_configured(base: &str, token: &str, title: &str, content: &str) -> Result<(), String> {
    let app_id = base
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("system_alerts")
        .to_string();
    let root = base.trim_end_matches(&format!("/{}", app_id));
    let url = format!("{root}/api/v1/notify/{app_id}");
    let payload = json!({
        "title": title,
        "content": content,
    });
    let _ = ureq::post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(payload.to_string())
        .map_err(|err| err.to_string())?;
    Ok(())
}

pub(crate) async fn get_config(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
) -> Response {
    let store = state.store.lock();
    let is_admin = user_id
        .and_then(|uid| store.get_user(uid).ok().flatten())
        .map(|u| u.role == domain::UserRole::Admin)
        .unwrap_or(false);
    ok(get_config_json(&store, is_admin)).into_response()
}

pub(crate) async fn put_config(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let url = body["notify_url"].as_str().unwrap_or_default();
    let token = body["notify_token"].as_str().unwrap_or_default();
    let store = state.store.lock();
    if let Err(error) = save_config(&store, url, token) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(get_config_json(&store, true)).into_response()
}

pub(crate) async fn test(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let url = body["notify_url"].as_str().unwrap_or_default();
    let token = body["notify_token"].as_str().unwrap_or_default();
    let store = state.store.lock();
    let base = if url.is_empty() {
        notify_url(&store).unwrap_or_default()
    } else {
        url.to_string()
    };
    let token = if token.is_empty() {
        notify_token(&store).unwrap_or_default()
    } else {
        token.to_string()
    };
    if base.is_empty() || token.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "notify.unconfigured",
            "请先配置 notify_url 和 notify_token",
        );
    }
    drop(store);
    // Direct sync send (ureq is fast; config errors already handled).
    match send_configured(&base, &token, "crawler-media 测试通知", "通知配置已生效") {
        Ok(_) => ok(json!({ "ok": true, "sent": true })).into_response(),
        Err(error) => ok(json!({ "ok": false, "sent": false, "error": error })).into_response(),
    }
}
