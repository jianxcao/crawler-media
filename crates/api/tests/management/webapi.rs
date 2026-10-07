use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::body::Body;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::Service;
use tower::ServiceExt;

use super::common::*;

fn authed_app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

#[path = "webapi/authentication.rs"]
mod authentication;
#[path = "webapi/resources.rs"]
mod resources;
#[path = "webapi/search.rs"]
mod search;

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(request(method, uri, token, body))
        .await
        .unwrap()
}
