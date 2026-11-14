use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

mod config;
mod tasks;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

async fn post_dl(app: &axum::Router, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED, "{response:?}");
    json_data(response).await
}
