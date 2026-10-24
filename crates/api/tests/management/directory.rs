//! Removed aggregate routes cannot mutate settings or roots.
use super::common::*;
use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;

#[tokio::test]
async fn directory_aggregate_and_root_routes_are_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let before = state.store().lock().list_library_roots().unwrap();
    let app = router(state.clone());
    for (method, path) in [
        ("GET", "/api/v1/directory"),
        ("PUT", "/api/v1/directory"),
        ("POST", "/api/v1/directory/roots"),
        ("DELETE", "/api/v1/directory/roots/unused"),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                method,
                path,
                Some("management-secret"),
                json!({"movie_root":"/outside"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    assert_eq!(state.store().lock().list_library_roots().unwrap(), before);
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/settings/transfer",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(response).await["revision"], 0);
}
