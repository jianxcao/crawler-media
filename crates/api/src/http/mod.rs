//! Self-hosted REST API (`/api/v1`), contract in
//! `docs/api-contracts/self.md`. No upstream compatibility: UUID ids,
//! unified `{ok, data}` / `{ok, error}` envelope, domain-shaped DTOs.
//!
//! Module layout mirrors the contract resources:
//! `auth` / `users` / `sites` / `search` / `rule_sets` / `subscriptions` /
//! `downloaders` / `library` / `jobs` / `directory` / `notify` / `playback`.
//!
//! Route assembly lives in the private `routes` child module; the request
//! observation middleware lives in `request_logging`.

pub(crate) mod auth;
pub(crate) mod browser_settings;
pub(crate) mod collections;
pub(crate) mod discover;
pub(crate) mod downloaders;
pub(crate) mod file_delete;
pub(crate) mod fs;
pub(crate) mod image_proxy;
pub(crate) mod job_maintenance;
pub(crate) mod jobs;
pub(crate) mod jobs_read;
pub(crate) mod library;
pub(crate) mod library_admin;
pub(crate) mod library_artwork;
pub(crate) mod library_chapters;
pub(crate) mod library_config;
pub(crate) mod library_delete;
pub(crate) mod library_deletion;
pub(crate) mod library_duplicates;
pub(crate) mod library_gallery;
pub(crate) mod library_housekeeping;
pub(crate) mod library_operations;
pub(crate) mod library_organize;
pub(crate) mod library_organize_apply;
pub(crate) mod library_presence;
pub(crate) mod library_scan;
pub(crate) mod media;
pub(crate) mod media_operations;
pub(crate) mod media_posters;
pub(crate) mod metadata_language;
pub(crate) mod notify;
pub(crate) mod playback;
pub(crate) mod playback_activity;
pub(crate) mod playback_device_target;
pub(crate) mod playback_logs;
pub(crate) mod playback_stats;
pub(crate) mod playback_views;
pub(crate) mod proxy_settings;
pub(crate) mod reidentify;
pub(crate) mod request_observation;
pub(crate) mod routing_preview;
pub(crate) mod rule_sets;
pub(crate) mod scrape_settings;
pub(crate) mod search;
pub(crate) mod settings;
pub(crate) mod sites;
pub(crate) mod store_wait;
pub(crate) mod strict_json;
pub(crate) mod subscribe_schedule;
pub(crate) mod subscription_depth;
pub(crate) mod subscription_missing;
pub(crate) mod subscriptions;
pub(crate) mod system_logs;
pub(crate) mod title_ref;
pub(crate) mod transfer_settings;
pub(crate) mod users;

mod request_logging;
mod routes;
pub(crate) use routes::protocol_write_gate;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::management::ApiState;

pub use request_logging::request_logger;

/// Success envelope: `{"ok": true, "data": ...}`.
/// 管理员授权在路由层收口：admin Router 统一挂 `auth::require_admin`
/// 中间件（authenticate 之后运行），成员请求在进入 handler 前就被 403，
/// 不再依赖每个 handler 自己记得调用检查。
pub(crate) fn ok(data: Value) -> Json<Value> {
    Json(json!({ "ok": true, "data": data }))
}

/// Success envelope for a plain list payload.
pub(crate) fn ok_list(data: Vec<Value>) -> Json<Value> {
    ok(Value::Array(data))
}

/// Error envelope: `{"ok": false, "error": {"code", "message"}}`.
pub(crate) fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

pub(crate) async fn health(
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> Response {
    // Review N-2: `read_only` is true while another instance holds the
    // data-directory lease (background jobs and write endpoints paused).
    ok(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": env!("GIT_HASH"),
        "read_only": !state.instance_writable(),
    }))
    .into_response()
}

pub(crate) fn router(state: ApiState) -> Router {
    // 生产路径：阈值在启动时从 `ServerConfig` 读一次，再作为 layer state 注入。
    router_with_slow_after(
        state,
        std::time::Duration::from_millis(crate::config::slow_request_ms()),
    )
}

/// Router with an injected slow-request threshold. Tests pass an explicit value
/// instead of mutating the process environment (which would race).
pub fn router_with_slow_after(state: ApiState, slow_after: std::time::Duration) -> Router {
    let public = public_routes();
    let member = routes::member_routes(&state);
    let admin = routes::admin_routes(&state);

    let protected = member
        .merge(admin)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
        .route_layer(axum::middleware::from_fn(query_timing));

    let base = public.merge(protected).with_state(state);
    base.layer(axum::middleware::from_fn_with_state(
        slow_after,
        request_logger,
    ))
    .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024))
}

async fn query_timing(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path();
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    let covered = matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && path.starts_with("/libraries/")
        && (path.ends_with("/items")
            || path.ends_with("/item-index")
            || path.ends_with("/facets")
            || path.ends_with("/gallery"));
    if covered
        && request
            .extensions()
            .get::<media_server::provider::QueryTiming>()
            .is_none()
    {
        let timing = request
            .extensions_mut()
            .remove::<media_server::provider::QueryTimingSeed>()
            .map(|seed| seed.0)
            .unwrap_or_else(media_server::provider::QueryTiming::start);
        request.extensions_mut().insert(timing);
    }
    media_server::query_metrics::observe_request(request, next).await
}

fn public_routes() -> Router<ApiState> {
    Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/health", get(health))
        .route("/images/proxy", get(image_proxy::proxy_image))
        // 图片资源（海报/背板/剧照/章节图/媒体库封面）走 <img> 加载，无法携带
        // Authorization 头（前端使用 Bearer token，不依赖 session cookie）——必须公开，
        // 否则封面与剧照永远 401 占位。
        .route("/media/{id}/poster", get(media_posters::get_media_poster))
        .route("/libraries/{id}/cover", get(library_artwork::library_cover))
        .route("/posters/{ledger_id}", get(library_artwork::poster))
        .route("/fanart/{ledger_id}", get(library_artwork::fanart))
        .route("/stills/{ledger_id}", get(library_artwork::stills))
        .route(
            "/chapters/{ledger_id}/{index}",
            get(library_chapters::chapter_image),
        )
}
