//! Downloaders: qBittorrent / Transmission instances, live tasks, submit.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::DownloaderId;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

use super::tasks::{DownloaderWorkError, run_downloader_action, run_downloader_read};

#[derive(Deserialize)]
pub(crate) struct DownloaderInput {
    name: String,
    kind: String,
    url: String,
    #[serde(default)]
    auth_type: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    path_maps: Vec<PathMapInput>,
    #[serde(default)]
    is_default: bool,
    /// Optional toggle; absent means "leave unchanged".
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct PathMapInput {
    from: String,
    to: String,
}

fn downloader_json_with_meta(
    row: &crate::store::DownloaderRow,
    status: &str,
    last_error: Option<String>,
) -> Value {
    let has_api_key = row.api_key.as_deref().is_some_and(|k| !k.trim().is_empty());
    let has_password = row.password.as_deref().is_some_and(|p| !p.is_empty());
    let auth_type = if row.kind == "qbittorrent" && has_api_key {
        "apikey"
    } else {
        "password"
    };
    json!({
        "id": row.id.to_string(),
        "name": row.name,
        "kind": row.kind,
        "url": row.url,
        "username": row.username,
        "auth_type": auth_type,
        "has_api_key": has_api_key,
        "has_password": has_password,
        "category": row.category,
        "path_maps": row.path_maps.iter().map(|m| json!({"from": m.from, "to": m.to.display().to_string()})).collect::<Vec<_>>(),
        "is_default": row.is_default,
        "enabled": row.enabled,
        "status": status,
        "last_error": last_error,
    })
}

fn downloader_status(enabled: bool, verified: Option<&str>) -> &'static str {
    if !enabled {
        "disabled"
    } else if verified == Some("true") {
        "active"
    } else if verified == Some("false") {
        "failed"
    } else {
        "pending"
    }
}

fn downloader_json_with_store(row: &crate::store::DownloaderRow, store: &crate::Store) -> Value {
    let verified = store
        .get_setting(&format!("downloader.{}.verified", row.id))
        .ok()
        .flatten();
    let last_error = store
        .get_setting(&format!("downloader.{}.last_error", row.id))
        .ok()
        .flatten();
    let status = downloader_status(row.enabled, verified.as_deref());
    downloader_json_with_meta(row, status, last_error)
}

fn validate_auth_type(kind: Option<&str>, auth_type: Option<&str>) -> Result<(), &'static str> {
    if let Some(auth) = auth_type {
        if !matches!(auth, "password" | "apikey") {
            return Err("auth_type 必须是 password 或 apikey");
        }
        if kind == Some("transmission") && auth == "apikey" {
            return Err("transmission 不支持 apikey 认证");
        }
    }
    Ok(())
}

fn downloader_json(row: &crate::store::DownloaderRow) -> Value {
    let status = if !row.enabled { "disabled" } else { "pending" };
    downloader_json_with_meta(row, status, None)
}

fn parse_path_maps(input: Vec<PathMapInput>) -> Vec<downloader::PathMap> {
    input
        .into_iter()
        .filter(|m| !m.from.is_empty() && !m.to.is_empty())
        .map(|m| downloader::PathMap::new(m.from, m.to))
        .collect()
}

pub(crate) async fn list_downloaders(State(state): State<ApiState>) -> Response {
    crate::http::store_wait::store_read(&state, &[store::Db::App], move |store| {
        let rows = match store.list_downloaders() {
            Ok(rows) => rows,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        ok_list(
            rows.iter()
                .map(|r| downloader_json_with_store(r, &store))
                .collect(),
        )
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn create_downloader(
    State(state): State<ApiState>,
    Json(body): Json<DownloaderInput>,
) -> Response {
    if body.name.trim().is_empty() || body.url.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "downloader.invalid",
            "名称和 URL 必填",
        );
    }
    if !matches!(body.kind.as_str(), "qbittorrent" | "transmission") {
        return err(
            StatusCode::BAD_REQUEST,
            "downloader.invalid",
            "kind 必须是 qbittorrent 或 transmission",
        );
    }
    if let Err(msg) = validate_auth_type(Some(body.kind.as_str()), body.auth_type.as_deref()) {
        return err(StatusCode::BAD_REQUEST, "downloader.invalid", msg);
    }
    let is_qbit = body.kind == "qbittorrent";
    let force_password = body.auth_type.as_deref() == Some("password");
    let force_apikey = is_qbit && body.auth_type.as_deref() == Some("apikey");
    let api_key = (is_qbit && !force_password)
        .then_some(body.api_key)
        .flatten()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    let username = (!force_apikey)
        .then_some(body.username)
        .flatten()
        .filter(|u| !u.is_empty());
    let password = (!force_apikey)
        .then_some(body.password)
        .flatten()
        .filter(|p| !p.is_empty());
    let row = crate::store::DownloaderRow {
        id: DownloaderId::new(),
        name: body.name,
        kind: body.kind,
        url: body.url,
        username,
        password,
        api_key,
        category: body.category.filter(|c| !c.is_empty()),
        path_maps: parse_path_maps(body.path_maps),
        is_default: body.is_default,
        enabled: body.enabled.unwrap_or(true),
    };
    crate::http::store_wait::store_write(&state, move |store| {
        if let Err(error) = store.insert_downloader(&row) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        (StatusCode::CREATED, ok(downloader_json(&row))).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}

pub(crate) async fn get_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    crate::http::store_wait::store_read(&state, &[store::Db::App], move |store| {
        match store.get_downloader(id) {
            Ok(Some(row)) => ok(downloader_json_with_store(&row, &store)).into_response(),
            Ok(None) => err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在"),
            Err(error) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ),
        }
    })
    .await
    .unwrap_or_else(|error| error.response())
}

#[derive(Deserialize)]
pub(crate) struct DownloaderPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    auth_type: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    path_maps: Option<Vec<PathMapInput>>,
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn apply_downloader_patch(
    row: &mut store::DownloaderRow,
    body: DownloaderPatch,
) -> Result<(), store::StoreError> {
    if let Some(name) = body.name.filter(|name| !name.is_empty()) {
        row.name = name;
    }
    if let Some(url) = body.url.filter(|url| !url.is_empty()) {
        row.url = url;
    }
    if let Some(kind) = body.kind.filter(|kind| !kind.is_empty()) {
        row.kind = kind;
    }
    if let Err(msg) = validate_auth_type(Some(row.kind.as_str()), body.auth_type.as_deref()) {
        return Err(store::StoreError::Invalid(msg.into()));
    }
    let explicit_username = body.username.is_some();
    if let Some(username) = body.username {
        row.username = (!username.is_empty()).then_some(username);
    }
    if let Some(password) = body.password {
        row.password = (!password.is_empty()).then_some(password);
    }
    if let Some(api_key) = body.api_key {
        let trimmed = api_key.trim();
        row.api_key = (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    if row.kind != "qbittorrent" {
        row.api_key = None;
    } else {
        match body.auth_type.as_deref() {
            Some("password") => row.api_key = None,
            Some("apikey") => {
                row.password = None;
                if !explicit_username {
                    row.username = None;
                }
            }
            _ => {}
        }
    }
    if let Some(category) = body.category {
        row.category = (!category.is_empty()).then_some(category);
    }
    if let Some(path_maps) = body.path_maps {
        row.path_maps = parse_path_maps(path_maps);
    }
    if let Some(enabled) = body.enabled {
        row.enabled = enabled;
    }
    Ok(())
}

pub(crate) async fn patch_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<DownloaderPatch>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    if let Some(kind) = body.kind.as_deref().filter(|kind| !kind.is_empty())
        && !matches!(kind, "qbittorrent" | "transmission")
    {
        return err(
            StatusCode::BAD_REQUEST,
            "downloader.invalid",
            "kind 必须是 qbittorrent 或 transmission",
        );
    }
    if let Err(msg) = validate_auth_type(body.kind.as_deref(), body.auth_type.as_deref()) {
        return err(StatusCode::BAD_REQUEST, "downloader.invalid", msg);
    }
    let make_default = body.is_default == Some(true);
    crate::http::store_wait::store_write(&state, move |store| {
        match store.mutate_downloader(id, make_default, |row| apply_downloader_patch(row, body)) {
            Ok(Some((row, verified, last_error))) => {
                let status = downloader_status(row.enabled, verified.as_deref());
                ok(downloader_json_with_meta(&row, status, last_error)).into_response()
            }
            Ok(None) => err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在"),
            Err(store::StoreError::Invalid(message)) => {
                err(StatusCode::BAD_REQUEST, "downloader.invalid", &message)
            }
            Err(error) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ),
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

pub(crate) async fn delete_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let orphaned = match crate::http::store_wait::store_write(&state, move |store| {
        let _ = store.delete_setting(&format!("downloader.{id}.verified"));
        let _ = store.delete_setting(&format!("downloader.{id}.last_error"));
        match store.delete_downloader(id) {
            Ok(true) => Ok(store.mark_pending_orphaned(id).unwrap_or_else(|error| {
                tracing::warn!(%id, %error, "标记已删除下载器的 pending 失败");
                0
            })),
            _ => Err(err(
                StatusCode::NOT_FOUND,
                "downloader.missing",
                "下载器不存在",
            )),
        }
    })
    .await
    {
        Ok(Ok(orphaned)) => orphaned,
        Ok(Err(response)) | Err(response) => return response,
    };
    if orphaned > 0 {
        tracing::warn!(%id, orphaned, "下载器已删除，其在途任务标记为 orphan");
        let worker = state.clone();
        if let Err(error) = state.http_work.run_to_completion(move || {
            crate::worker::notify(&worker, "下载器已删除", &format!(
                "{orphaned} 个在途订阅任务失去下载器，已停止跟踪，可重新搜索提交到其它下载器"
            ));
        }).await {
            tracing::warn!(%id, %error, "下载器删除通知未能调度");
        }
    }
    ok(json!({ "deleted": true, "orphaned_pending": orphaned })).into_response()
}

mod verification;
pub(crate) use verification::verify_downloader;

mod preferences;
pub(crate) use preferences::{put_target_pref, target_prefs};

#[derive(Deserialize, Default)]
pub(crate) struct LimitsQuery {
    id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SetLimitsInput {
    #[serde(default)]
    download_limit_bytes: Option<Value>,
    #[serde(default)]
    upload_limit_bytes: Option<Value>,
}

fn parse_limit_val(val: Option<&Value>) -> Result<u64, &'static str> {
    match val {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                Ok(u)
            } else {
                Err("限速数值必须为非负整数")
            }
        }
        _ => Err("限速格式不正确，必须为数字或 null"),
    }
}

/// 只解析 query 里的下载器 id；client 解析必须留到 blocking 闭包内（同步建连）。
fn parse_downloader_id(
    id: Option<&str>,
) -> Result<Option<DownloaderId>, (StatusCode, &'static str, String)> {
    match id.filter(|s| !s.is_empty()) {
        Some(raw) => Ok(Some(DownloaderId::from_str(raw).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "downloader.invalid_id",
                "下载器 ID 不是合法 UUID".to_string(),
            )
        })?)),
        None => Ok(None),
    }
}

/// GET /downloaders/limits
pub(crate) async fn get_limits(
    State(state): State<ApiState>,
    Query(query): Query<LimitsQuery>,
) -> Response {
    let downloader_id = match parse_downloader_id(query.id.as_deref()) {
        Ok(id) => id,
        Err((status, code, msg)) => return err(status, code, &msg),
    };
    if let Some(id) = downloader_id {
        match crate::http::store_wait::store_read(&state, &[store::Db::App], move |store| {
            store.get_downloader(id)
        })
        .await
        {
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) => {
                return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在");
            }
            Ok(Err(error)) => {
                tracing::error!(%id, %error, "读取下载器配置失败");
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
            Err(error) => return error.response(),
        }
    }
    match run_downloader_read(&state, downloader_id, |client| client.get_speed_limits()).await {
        Ok((dl, ul)) => ok(json!({
            "status": "available",
            "error": null,
            "download_limit_bytes": if dl == 0 { Value::Null } else { json!(dl) },
            "upload_limit_bytes": if ul == 0 { Value::Null } else { json!(ul) },
        }))
        .into_response(),
        Err(DownloaderWorkError::Resolve(message) | DownloaderWorkError::Operation(message)) => {
            unavailable_limits(downloader_id, &message)
        }
        Err(DownloaderWorkError::Timeout) => unavailable_limits(downloader_id, "下载器响应超时"),
        Err(error) => error.response(),
    }
}

fn unavailable_limits(id: Option<DownloaderId>, message: &str) -> Response {
    tracing::warn!(downloader_id = ?id, error = %message, "下载器限速状态不可用");
    ok(json!({
        "status": "unavailable",
        "error": message,
        "download_limit_bytes": null,
        "upload_limit_bytes": null,
    }))
    .into_response()
}

/// PUT /downloaders/limits
pub(crate) async fn set_limits(
    State(state): State<ApiState>,
    Query(query): Query<LimitsQuery>,
    Json(body): Json<SetLimitsInput>,
) -> Response {
    let dl = match parse_limit_val(body.download_limit_bytes.as_ref()) {
        Ok(v) => v,
        Err(msg) => return err(StatusCode::BAD_REQUEST, "downloader.invalid_limit", msg),
    };
    let ul = match parse_limit_val(body.upload_limit_bytes.as_ref()) {
        Ok(v) => v,
        Err(msg) => return err(StatusCode::BAD_REQUEST, "downloader.invalid_limit", msg),
    };
    let downloader_id = match parse_downloader_id(query.id.as_deref()) {
        Ok(id) => id,
        Err((status, code, msg)) => return err(status, code, &msg),
    };
    // 有副作用：run_to_completion 保证返回时动作已真正结束。
    match run_downloader_action(&state, downloader_id, move |client| {
        client.set_speed_limits(dl, ul)
    })
    .await
    {
        Ok(()) => ok(json!({
            "status": "available",
            "error": null,
            "download_limit_bytes": if dl == 0 { Value::Null } else { json!(dl) },
            "upload_limit_bytes": if ul == 0 { Value::Null } else { json!(ul) },
        }))
        .into_response(),
        Err(error) => error.response(),
    }
}
