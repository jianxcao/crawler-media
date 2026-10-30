//! Artwork: cover candidates, select, upload, and the poster byte endpoint.
//! Covers are stored as `poster.jpg` beside the owning file; a per-Media
//! `artwork.{media_id}` setting locks a manual selection so automatic
//! refreshes never overwrite it (Emby-style artwork lock).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

// 从 library_artwork.rs 注册兄弟文件模块，避免改动本任务写范围之外的 `http/mod.rs`。
#[path = "image_response.rs"]
pub(crate) mod image_response;

mod images;
mod writes;
pub(crate) use images::{fanart, library_cover, poster, stills};
pub(crate) use writes::{select_artwork, upload_artwork};

use domain::MediaId;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::blocking_budget::BlockingError;
use crate::http::{err, ok};
use crate::management::ApiState;

use super::library::{poster_path, preferred_coverage, require_visible_library};
use super::store_wait;

fn artwork_key(media_id: MediaId) -> String {
    format!("artwork.{}", media_id)
}

fn poster_locked(store: &crate::Store, media_id: MediaId) -> bool {
    store
        .get_setting(&artwork_key(media_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value["poster_locked"].as_bool())
        .unwrap_or(false)
}

/// GET /libraries/{id}/items/{item_id}/artwork/candidates
pub(crate) async fn artwork_candidates(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };

    // 将数据库快照查询与文件/网络 I/O 隔离在 blocking 预算池中执行，避免阻塞 Tokio async worker
    let state_for_blocking = state.clone();
    let result = state
        .http_work
        .run(std::time::Duration::from_secs(10), move || {
            let store = state_for_blocking.store.clone();
            let library = match require_visible_library(&store, &id, user_id) {
                Ok(library) => library,
                Err(response) => return Err(response),
            };
            let Some(media) = store.get_media(media_id).ok().flatten() else {
                return Err(err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"));
            };
            let rows: Vec<domain::FileCoverage> = store.file_coverage_for_media(media_id).unwrap_or_default().into_iter()
                .filter(|coverage| coverage.file.library_id.to_string() == library.id)
                .collect();
            if rows.is_empty() {
                return Err(err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"));
            }
            let selected = preferred_coverage(&rows).cloned();
            let locked = poster_locked(&store, media_id);
            // 及时释放 Store 锁，再做后续文件/网络检查
            drop(store);

            let current = selected
                .as_ref()
                .and_then(|coverage| poster_path(&coverage.file))
                .map(|p| p.display().to_string());

            let mut posters: Vec<Value> = Vec::new();
            if let (Some(path), Some(coverage)) = (&current, &selected) {
                posters.push(json!({
                    "file_path": path,
                    "preview_url": crate::http::library::artwork_url("posters", coverage.file.file_id),
                    "width": 342,
                    "height": 513,
                    "language": null,
                    "vote_average": null,
                    "vote_count": null,
                }));
            }
            // Remote metadata candidate
            if let Some(tmdb_id) = media.tmdb_id.clone()
                && let Ok(Some(url)) = state_for_blocking.catalog.cached_poster_url(media.kind, &tmdb_id) {
                    posters.push(json!({
                        "file_path": url,
                        "preview_url": url,
                        "width": null,
                        "height": null,
                        "language": null,
                        "vote_average": null,
                        "vote_count": null,
                    }));
                }
            Ok(json!({
                "posters": posters,
                "backdrops": [],
                "current_poster": current,
                "current_backdrop": null,
                "poster_locked": locked,
                "backdrop_locked": false,
            }))
        })
        .await;

    match result {
        Ok(Ok(val)) => ok(val).into_response(),
        Ok(Err(resp)) => resp,
        Err(BlockingError::Busy) => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请稍后重试",
        ),
        Err(BlockingError::Timeout) => err(
            StatusCode::GATEWAY_TIMEOUT,
            "library.artwork_timeout",
            "获取海报候选超时",
        ),
        Err(BlockingError::Worker(msg)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "library.artwork_failed",
            &msg,
        ),
    }
}

pub(crate) fn image_content_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else {
        // Existing artwork paths have a `.jpg` contract. The explicit PNG
        // and WebP signatures above override it for uploaded alternatives.
        "image/jpeg"
    }
}

/// POST /libraries/{id}/cover — 用户主动上传/更换媒体库封面（JPEG/PNG/WebP base64 data URL）
pub(crate) async fn upload_library_cover(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    use base64::Engine;
    let library_id = match visible_library(&state, id, user_id).await {
        Ok(library) => library.id,
        Err(response) => return response,
    };

    let data_url = body["data_url"].as_str().unwrap_or_default();
    let (_, encoded) = match data_url.split_once(',') {
        Some(parts) => parts,
        None => return err(StatusCode::BAD_REQUEST, "cover.upload", "图片格式无效"),
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= 12 * 1024 * 1024 => bytes,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "cover.upload",
                "图片为空或超过 12MB",
            );
        }
        Err(_) => return err(StatusCode::BAD_REQUEST, "cover.upload", "图片编码无效"),
    };

    let published = match state
        .cover_service
        .replace_cover(&state, &library_id, bytes.clone())
        .await
    {
        Ok(path) => path,
        Err(error) => {
            let status = if error.starts_with("runtime.busy") {
                StatusCode::SERVICE_UNAVAILABLE
            } else if error.starts_with("cover.stale") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return err(status, "cover.write", &error);
        }
    };
    ok(json!({ "bytes": bytes.len(), "path": published.display().to_string() })).into_response()
}

/// DELETE /libraries/{id}/cover — 删除/清空媒体库自定义封面
pub(crate) async fn delete_library_cover(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
) -> Response {
    let library_id = match visible_library(&state, id, user_id).await {
        Ok(library) => library.id,
        Err(response) => return response,
    };

    match state.cover_service.remove_cover(&state, &library_id).await {
        Ok(()) => {}
        Err(error) => {
            let status = if error.starts_with("runtime.busy") {
                StatusCode::SERVICE_UNAVAILABLE
            } else if error.starts_with("cover.stale") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return err(status, "cover.delete", &error);
        }
    }
    ok(json!({ "deleted": true })).into_response()
}

/// POST /libraries/{id}/cover/generate — 自动/手动重新生成媒体库艺术封面
pub(crate) async fn generate_library_cover(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    use base64::Engine;
    use cover_generator::{BackgroundOption, CoverOptions, CoverStyle};

    let library = match visible_library(&state, id, user_id).await {
        Ok(library) => library,
        Err(response) => return response,
    };

    let title_zh = body["title_zh"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| library.name.clone());
    let title_en = body["title_en"].as_str().map(str::to_string);
    let preview_only = body["preview_only"].as_bool().unwrap_or(false);
    let style = match body["style"].as_str() {
        Some("multi_poster_pile") | Some("multi") => CoverStyle::MultiPosterPile,
        _ => CoverStyle::MacaronCardSingle,
    };
    let background: BackgroundOption =
        serde_json::from_value(body["background"].clone()).unwrap_or_default();
    let options = CoverOptions {
        title_zh,
        title_en,
        width: 1920,
        height: 1080,
        style,
        background,
    };

    let generated = match state
        .cover_service
        .generate(&state, &library.id, options, preview_only)
        .await
    {
        Ok(generated) => generated,
        Err(error) if error.starts_with("cover.empty") => {
            return err(
                StatusCode::BAD_REQUEST,
                "cover.empty",
                "媒体库内无可用海报图片",
            );
        }
        Err(error) if error.starts_with("cover.stale") => {
            return err(StatusCode::CONFLICT, "cover.stale", &error);
        }
        Err(error) if error.starts_with("runtime.busy") => return store_wait::busy_response(),
        Err(error) => return err(StatusCode::INTERNAL_SERVER_ERROR, "cover.error", &error),
    };

    let data_url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&generated.bytes)
    );
    if preview_only {
        return ok(json!({ "data_url": data_url, "preview": true })).into_response();
    }
    ok(json!({
        "success": true,
        "cover_url": format!("/libraries/{}/cover", library.id),
        "data_url": data_url,
    }))
    .into_response()
}

async fn visible_library(
    state: &ApiState,
    id: String,
    user: Option<domain::UserId>,
) -> Result<store::Library, Response> {
    store_wait::store_read(state, move |store| {
        require_visible_library(store, &id, user)
    })
    .await
    .map_err(|error| error.response())?
}
