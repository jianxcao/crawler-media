use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn api_app(root: &Path) -> axum::Router {
    router(
        ApiState::new(
            Store::open(root.join("data")).unwrap(),
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    )
}

fn write_ui(root: &Path) -> std::path::PathBuf {
    let ui = root.join("ui");
    std::fs::create_dir_all(ui.join("assets")).unwrap();
    std::fs::write(ui.join("index.html"), "<html>crawler-media ui</html>").unwrap();
    std::fs::write(ui.join("assets/app.js"), "console.log('app')").unwrap();
    ui
}

#[tokio::test]
async fn cors_preflight_allows_dev_origin() {
    let app = Router::new()
        .route("/ping", get(|| async {}))
        .layer(api::ui::cors_layer(&["http://127.0.0.1:3334".into()]));
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/ping")
                .header("origin", "http://127.0.0.1:3334")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://127.0.0.1:3334"
    );
}

#[tokio::test]
async fn cors_headers_on_simple_api_request() {
    let tmp = tempfile::tempdir().unwrap();
    let app = api_app(tmp.path());
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/health")
                .header("origin", "http://127.0.0.1:3334")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://127.0.0.1:3334"
    );
}

#[tokio::test]
async fn cors_rejects_unknown_origin() {
    let app = Router::new()
        .route("/ping", get(|| async {}))
        .layer(api::ui::cors_layer(&["http://127.0.0.1:3334".into()]));
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ping")
                .header("origin", "http://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
}

#[tokio::test]
async fn ui_serves_index_assets_and_spa_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let ui = write_ui(tmp.path());
    let app = api::ui::attach_ui(api_app(tmp.path()), Some(ui));

    let index = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let bytes = to_bytes(index.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"<html>crawler-media ui</html>");

    let asset = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    let bytes = to_bytes(asset.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"console.log('app')");

    // SPA fallback:未知非 API 路径返回 index.html 交给前端路由。
    let spa = app
        .oneshot(
            Request::builder()
                .uri("/library/abc-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(spa.status(), StatusCode::OK);
    let bytes = to_bytes(spa.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"<html>crawler-media ui</html>");
}

#[tokio::test]
async fn api_unknown_paths_stay_json_404() {
    let tmp = tempfile::tempdir().unwrap();
    let ui = write_ui(tmp.path());
    let app = api::ui::attach_ui(api_app(tmp.path()), Some(ui));

    let api = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/definitely-not-a-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(api.status(), StatusCode::NOT_FOUND);
    assert!(
        api.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("json")
    );

    // Jellyfin 命名空间未实现端点:JSON 404,不能吐 HTML 给播放器。
    let jellyfin = app
        .oneshot(
            Request::builder()
                .uri("/Items/not-a-real-item/SubtitleStream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(jellyfin.status(), StatusCode::NOT_FOUND);
    assert!(
        jellyfin.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("json")
    );
}

#[tokio::test]
async fn without_ui_dir_keeps_plain_api_404() {
    let tmp = tempfile::tempdir().unwrap();
    let app = api::ui::attach_ui(api_app(tmp.path()), None);
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
