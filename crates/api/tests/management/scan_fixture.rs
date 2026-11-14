//! Complete asynchronous creation scans before writing a fixture's chosen identity.
use axum::{Router, http::StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::{json_data, request};

pub(crate) async fn finish_scan(app: &Router, library: &str) {
    let started = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{library}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::ACCEPTED);
    let started = json_data(started).await;
    let id = started["operation_id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library}/scan-operations/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        let status = json_data(response).await;
        if status["status"] == "completed" {
            return;
        }
        assert!(
            status["status"] != "failed" && status["status"] != "cancelled",
            "{status}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "Scan fixture timed out: {status}"
        );
        tokio::task::yield_now().await;
    }
}
