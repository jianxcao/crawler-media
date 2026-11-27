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

#[tokio::test]
async fn plugin_task_definitions_are_admin_only_in_jobs_api() {
    let tmp = tempfile::tempdir().unwrap();
    let dl = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let fixtures = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let state = state(tmp.path(), fixtures, dl);
    let binding_id = domain::PluginTaskBindingId::new();
    let def = state
        .jobs()
        .acquire()
        .unwrap()
        .create_def(jobs::NewDef {
            kind: jobs::JobKind::PluginTask,
            scope: jobs::JobScope::PluginTask(binding_id),
            name: "plugin:org.example:checkin".into(),
            enabled: true,
            schedule: Some(jobs::Schedule::Interval { secs: 600 }),
            payload: serde_json::json!({ "binding_id": binding_id }).to_string(),
            timeout_secs: Some(60),
            concurrency_key: None,
        })
        .unwrap();
    let app = router(state);
    let (_, member_token) = create_member(&app, "plugin-job-member").await;

    let admin_list = json_body(
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
    assert!(
        admin_list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == def.id.to_string())
    );
    let admin_get = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/jobs/{}", def.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(admin_get.status(), StatusCode::OK);

    let member_list = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/jobs",
                Some(&member_token),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !member_list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == def.id.to_string())
    );
    let member_get = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/jobs/{}", def.id),
            Some(&member_token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(member_get.status(), StatusCode::NOT_FOUND);
}
