use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, create_api_subscribe, json_body, request, state};

#[tokio::test]
async fn failed_job_schedule_sync_rolls_back_the_subscription_update() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let created = create_api_subscribe(&app, "Schedule Rollback", "management-secret").await;
    let id = created["id"].as_str().unwrap();
    let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    jobs.execute_batch(&format!(
        "CREATE TRIGGER fail_subscribe_schedule BEFORE UPDATE OF schedule ON job_defs \
         WHEN OLD.payload = '{{\"subscribe_id\":\"{id}\"}}' \
         BEGIN SELECT RAISE(ABORT, 'schedule update rejected'); END;"
    ))
    .unwrap();

    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "search_interval_secs": 7200 }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let details = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(details["data"]["search_interval_secs"], 1800);
}
