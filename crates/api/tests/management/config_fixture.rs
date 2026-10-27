//! Test setup through current, separate settings and Library operations.
use axum::{Router, http::StatusCode, response::Response};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{json_data, request};

pub(crate) async fn set_transfer_settings(app: &Router, mut patch: Value) -> Response {
    let current = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/settings/transfer",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    patch["expected_revision"] = json_data(current).await["revision"].clone();
    app.clone()
        .oneshot(request(
            "PATCH",
            "/api/v1/settings/transfer",
            Some("management-secret"),
            patch,
        ))
        .await
        .unwrap()
}

async fn default_library(app: &Router, kind: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries?scope=admin",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    json_data(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == kind && row["is_default"] == true)
        .unwrap()
        .clone()
}

pub(crate) async fn set_default_root(app: &Router, kind: &str, root: &std::path::Path) {
    let library = default_library(app, kind).await;
    update_roots(app, library["id"].as_str().unwrap(), json!([root])).await;
}

pub(crate) async fn add_default_root(app: &Router, kind: &str, root: &std::path::Path) {
    let library = default_library(app, kind).await;
    let mut paths = library["root_paths"].as_array().unwrap().clone();
    paths.push(json!(root));
    update_roots(app, library["id"].as_str().unwrap(), json!(paths)).await;
}

async fn update_roots(app: &Router, id: &str, paths: Value) {
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{id}"),
            Some("management-secret"),
            json!({"root_paths":paths}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let operation = json_data(response).await;
    let operation = operation["operation_id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{id}/operations/{operation}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        let view = json_data(response).await;
        if view["state"] == "succeeded" {
            return;
        }
        assert_ne!(view["state"], "failed", "{view}");
        assert!(
            std::time::Instant::now() < deadline,
            "root operation timed out: {view}"
        );
        tokio::task::yield_now().await;
    }
}
