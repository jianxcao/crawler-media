//! CORS 与静态 UI 托管。
//!
//! 两种部署形态共用同一个 Rust 进程:
//! - 开发:Vite dev(3334)将 API 请求代理到后端(18765)。
//! - 生产:单端口,Rust 同时托管 `web/dist` 静态 UI 与 API(Jellyfin 兼容),
//!   同一来源下 CORS 头对浏览器无副作用,因此不必按环境区分。

use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::StatusCode;
use axum::http::header::{self, HeaderValue};
use axum::response::{IntoResponse, Response};
use tower::service_fn;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::ServeDir;

/// 开发环境默认放行的 origin(与 start-test.sh 的前端端口一致)。
const DEV_ORIGINS: [&str; 2] = ["http://127.0.0.1:3334", "http://localhost:3334"];

/// Jellyfin 兼容接口的命名空间首段(小写)。SPA fallback 时这些前缀仍返回
/// JSON 404,不让播放器把「未实现的端点」误当成 HTML 页面。
const JELLYFIN_NAMESPACES: [&str; 15] = [
    "system",
    "users",
    "userviews",
    "useritems",
    "userplayeditems",
    "userfavoriteitems",
    "items",
    "videos",
    "shows",
    "playingitems",
    "branding",
    "quickconnect",
    "emby",
    "plugins",
    "displaypreferences",
];

/// 按显式 origin 列表构造 CORS 层。鉴权走 `Authorization: Bearer` 头
/// (localStorage 存 token)，任务流 SSE 也用 fetch + Bearer 订阅（不再依赖
/// cookie），因此不开 allow_credentials，可以放心用通配方法/请求头；
/// 跨域拉流的 Range 预检也会通过。
pub fn cors_layer(origins: &[String]) -> CorsLayer {
    let list: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(list))
        .allow_methods(Any)
        .allow_headers(Any)
        .max_age(Duration::from_secs(86400))
}

/// 从环境变量读取 CORS 白名单;未设置时用开发默认端口。
pub fn cors_layer_from_env() -> CorsLayer {
    let parsed = crate::config::cors_origins_from_env();
    let origins = if parsed.is_empty() {
        DEV_ORIGINS.iter().map(|s| s.to_string()).collect()
    } else {
        parsed
    };
    cors_layer(&origins)
}

/// 若 `ui_dir` 存在,把静态 UI 挂到所有未匹配路径上(SPA fallback)。
/// 未设置或目录无效时原样返回,保持纯 API 行为。
pub fn attach_ui(app: Router, ui_dir: Option<PathBuf>) -> Router {
    let Some(dir) = ui_dir else {
        return app.route("/", axum::routing::get(|| async { StatusCode::NOT_FOUND }));
    };
    if !dir.is_dir() {
        eprintln!(
            "[ui] CRAWLER_MEDIA_UI={} 不是目录,跳过静态托管",
            dir.display()
        );
        return app;
    }
    let index = dir.join("index.html");
    let index_for_fallback = index.clone();
    let not_found = service_fn(move |request| {
        let index = index_for_fallback.clone();
        async move { Ok::<_, std::convert::Infallible>(spa_or_json_404(request, &index).await) }
    });
    // 注意用 .fallback() 而非 .not_found_service():后者会把 fallback 的
    // 状态码强制改成 404,SPA 分支的 200 会被覆盖; API / Jellyfin 路径在进入
    // ServeDir 前即返回 JSON 404,避免 PATCH/POST/PUT 等非 GET 方法被 ServeDir 拒为 405。
    let serve_dir = ServeDir::new(dir).fallback(not_found);
    let fallback = service_fn(move |request: axum::http::Request<Body>| {
        let serve_dir = serve_dir.clone();
        async move {
            if is_api_or_jellyfin_path(request.uri().path()) {
                return Ok::<_, std::convert::Infallible>(json_404_response());
            }
            use tower::ServiceExt as _;
            let resp = match serve_dir.oneshot(request).await {
                Ok(resp) => resp.into_response(),
                Err(err) => match err {},
            };
            Ok::<_, std::convert::Infallible>(resp)
        }
    });
    app.fallback_service(fallback)
}

fn is_api_or_jellyfin_path(path: &str) -> bool {
    let first = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    path.starts_with("/api/")
        || path == "/api"
        || path == "/sessions"
        || path.starts_with("/sessions/")
        || JELLYFIN_NAMESPACES.contains(&first.as_str())
}

fn json_404_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"error\":\"not found\"}",
    )
        .into_response()
}

/// 文件未命中时:API / Jellyfin 命名空间返回 JSON 404,其余返回 index.html
/// 交给前端路由(SPA)。
async fn spa_or_json_404(
    request: axum::http::Request<axum::body::Body>,
    index: &std::path::Path,
) -> Response {
    if is_api_or_jellyfin_path(request.uri().path()) {
        return json_404_response();
    }
    match tokio::fs::read(index).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            Body::from(bytes),
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn attach_ui_serves_plugin_apps_spa_and_keeps_jellyfin_plugins_404_json() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><html><body><div id=\"root\"></div></body></html>",
        )
        .unwrap();
        let app = attach_ui(
            Router::new().route("/api/v1/health", axum::routing::get(|| async { "ok" })),
            Some(dir.path().to_path_buf()),
        );

        let r_plugins = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/plugins/org.example.demo/checkin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_plugins.status(), StatusCode::NOT_FOUND);
        assert!(
            r_plugins
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .contains("application/json")
        );
        let b_plugins = to_bytes(r_plugins.into_body(), usize::MAX).await.unwrap();
        assert_eq!(b_plugins.as_ref(), br#"{"error":"not found"}"#);

        let r_patch_api = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/sites/00000000-0000-0000-0000-000000000000/boost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_patch_api.status(), StatusCode::NOT_FOUND);
        let b_patch_api = to_bytes(r_patch_api.into_body(), usize::MAX).await.unwrap();
        assert_eq!(b_patch_api.as_ref(), br#"{"error":"not found"}"#);

        for spa_path in [
            "/plugin-apps/org.example.demo/checkin",
            "/settings/plugins/builtin.checkin",
        ] {
            let r_spa = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(spa_path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r_spa.status(), StatusCode::OK, "path={spa_path}");
            assert!(
                r_spa
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .contains("text/html")
            );
            let b_spa = to_bytes(r_spa.into_body(), usize::MAX).await.unwrap();
            assert!(String::from_utf8_lossy(&b_spa).contains("<div id=\"root\">"));
        }
    }
}
