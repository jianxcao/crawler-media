//! Tests for downloader verification endpoint and status reflection.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state};

#[tokio::test]
async fn verify_downloader_fails_on_unreachable_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = router(api_state.clone());

    // 1. Create a downloader with unreachable local port
    let create_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders",
            Some("management-secret"),
            json!({
                "name": "unreachable-qb",
                "kind": "qbittorrent",
                "url": "http://127.0.0.1:49999",
                "username": "admin",
                "password": "wrong-password"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let created = json_data(create_resp).await;
    let dl_id = created["id"].as_str().unwrap().to_string();

    // 2. Listing shows pending status, NOT active
    let list_resp = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list = json_data(list_resp).await;
    let found = list
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == dl_id)
        .unwrap();
    assert_eq!(found["status"], "pending");

    // 3. POST /verify fails faithfully with ok: false and an error string
    let verify_resp = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/downloaders/{dl_id}/verify"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(verify_resp.status(), StatusCode::OK);
    let verify_data = json_data(verify_resp).await;
    assert_eq!(verify_data["ok"], false);
    assert!(verify_data["error"].as_str().is_some());

    // 4. Listing after failed verify reflects "failed" and populates last_error
    let list_resp2 = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(list_resp2.status(), StatusCode::OK);
    let list2 = json_data(list_resp2).await;
    let found2 = list2
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == dl_id)
        .unwrap();
    assert_eq!(found2["status"], "failed");
    assert!(found2["last_error"].as_str().is_some());
}
