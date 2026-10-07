//! Subscription contract regression tests, including PATCH filter_id.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state, subscribe_payload};

async fn create_filter_fixture(app: &axum::Router, name: &str, res: &str) -> String {
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": name,
                "atoms": [
                    { "kind": "resolution", "value": res, "priority": 100 }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json_data(resp).await["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn patch_subscription_filter_id_updates_store_and_rejects_invalid() {
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

    let _filter_a_id = create_filter_fixture(&app, "filter-a", "1080p").await;
    let filter_b_id = create_filter_fixture(&app, "filter-b", "2160p").await;

    // 2. Create subscription with Filter A
    let mut body = subscribe_payload("search");
    body["filter"] = json!({
        "name": "filter-a",
        "atoms": [
            { "kind": "resolution", "value": "1080p", "priority": 100 }
        ]
    });
    let create_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let sub = json_data(create_resp).await;
    let sub_id = sub["id"].as_str().unwrap().to_string();

    // 3. PATCH with Filter B
    let patch_resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "filter_id": filter_b_id }),
        ))
        .await
        .unwrap();
    assert_eq!(patch_resp.status(), StatusCode::OK);

    let store_binding = api_state.store();
    let current_sub = store_binding
        .lock()
        .get_subscribe(sub_id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(current_sub.filter_id.to_string(), filter_b_id);

    // 4. Reject invalid/non-existent or null filter_id
    let null_patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "filter_id": null }),
        ))
        .await
        .unwrap();
    assert_eq!(null_patch.status(), StatusCode::BAD_REQUEST);

    let non_existent_id = uuid::Uuid::new_v4().to_string();
    let bad_patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "filter_id": non_existent_id }),
        ))
        .await
        .unwrap();
    assert_eq!(bad_patch.status(), StatusCode::BAD_REQUEST);

    // 5. Reject unknown field
    let unknown_field_patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "filter_idx": filter_b_id }),
        ))
        .await
        .unwrap();
    assert_eq!(
        unknown_field_patch.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
