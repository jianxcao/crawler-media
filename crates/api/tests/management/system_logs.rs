use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use super::common::{Fixtures, json_body, state};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn system_logs_flow() {
    let dir = TempDir::new().unwrap();
    let fixtures = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let state = state(dir.path(), fixtures, downloader);

    state
        .log_buffer
        .push("INFO", "subscribe", "Scanning subscriptions".into());
    state
        .log_buffer
        .push("WARN", "downloader", "Download rate slow".into());
    state
        .log_buffer
        .push("ERROR", "indexer", "Site login cookie expired".into());

    let app = api::router(state.clone());

    // 1. Unauthenticated -> 401
    let unauthed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

    // 2. Authenticated as Admin -> 200 list
    let list_req = Request::builder()
        .uri("/api/v1/system/logs")
        .header(header::AUTHORIZATION, "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(list_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["data"]["total"], 3);
    let entries = body["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["level"], "INFO");
    assert_eq!(entries[1]["level"], "WARN");
    assert_eq!(entries[2]["level"], "ERROR");

    // 3. Filter by level
    let filter_req = Request::builder()
        .uri("/api/v1/system/logs?level=error")
        .header(header::AUTHORIZATION, "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(filter_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["data"]["total"], 1);
    assert_eq!(body["data"]["entries"][0]["target"], "indexer");

    // 4. Filter by keyword
    let search_req = Request::builder()
        .uri("/api/v1/system/logs?q=cookie")
        .header(header::AUTHORIZATION, "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(search_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["data"]["total"], 1);
    assert!(
        body["data"]["entries"][0]["message"]
            .as_str()
            .unwrap()
            .contains("cookie")
    );

    // 5. Export plain text
    let export_req = Request::builder()
        .uri("/api/v1/system/logs/export")
        .header(header::AUTHORIZATION, "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(export_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("Scanning subscriptions"));
    assert!(text.contains("Site login cookie expired"));

    // 6. Clear logs
    let clear_req = Request::builder()
        .uri("/api/v1/system/logs/clear")
        .method("POST")
        .header(header::AUTHORIZATION, "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(clear_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // After clear -> empty list
    let after_clear = Request::builder()
        .uri("/api/v1/system/logs")
        .header(header::AUTHORIZATION, "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(after_clear).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["data"]["total"], 0);
}
