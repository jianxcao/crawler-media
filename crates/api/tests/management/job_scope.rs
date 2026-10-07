use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::{Fixtures, create_api_subscribe, create_member, json_body, request, state};

#[tokio::test]
async fn member_job_reads_only_expose_owned_subscribe_search_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let (_, alice_token) = create_member(&app, "job-alice").await;
    let (_, bob_token) = create_member(&app, "job-bob").await;
    let alice = create_api_subscribe(&app, "Alice Private Title", &alice_token).await;
    let bob = create_api_subscribe(&app, "Bob Private Title", &bob_token).await;
    let alice_id = alice["id"].as_str().unwrap();
    let bob_id = bob["id"].as_str().unwrap();

    let all_jobs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/jobs",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let alice_job_id = all_jobs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["payload"]["subscribe_id"] == alice_id)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let bob_jobs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/jobs",
                Some(&bob_token),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let bob_rows = bob_jobs["data"].as_array().unwrap();
    assert!(
        bob_rows
            .iter()
            .any(|job| job["payload"]["subscribe_id"] == bob_id)
    );
    assert!(
        !bob_rows
            .iter()
            .any(|job| job["payload"]["subscribe_id"] == alice_id)
    );
    assert!(
        !bob_rows
            .iter()
            .any(|job| { job["friendly_name"] == "自动搜索《Alice Private Title》" })
    );

    let private_job = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/jobs/{alice_job_id}"),
            Some(&bob_token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(private_job.status(), StatusCode::NOT_FOUND);
}
