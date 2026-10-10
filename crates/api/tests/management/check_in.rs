use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn jobs_list_includes_check_in_def() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/jobs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let defs = json_data(response).await;
    let defs = defs.as_array().unwrap();
    assert!(defs.iter().any(|row| row["kind"] == "check_in"), "{defs:?}");
}

#[tokio::test]
async fn job_tick_runs_check_in_when_a_site_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    create_site(&app).await;

    let mut saw_check_in = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_data(response).await;
        let kinds = body["kinds"].as_array().unwrap();
        if kinds.iter().any(|kind| kind == "check_in") {
            saw_check_in = true;
            break;
        }
    }
    assert!(saw_check_in, "Check-in Job did not run");
}

#[tokio::test]
async fn failed_site_check_in_fails_the_scheduled_job() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let mut site = site_payload();
    site["cookie"] = Value::Null;
    site["url"] = Value::String("http://127.0.0.1:1".into());
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            site,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let mut saw_check_in = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        let body = json_data(response).await;
        if body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "check_in")
        {
            saw_check_in = true;
            break;
        }
    }
    assert!(saw_check_in, "Check-in Job did not run");

    let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    let (status, error): (String, String) = jobs
        .query_row(
            "SELECT status, error FROM jobs WHERE kind = 'check_in' ORDER BY run_after DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "queued");
    assert!(
        error.contains("请人工登录并配置有效 Cookie"),
        "缺 Cookie 必须给出可操作失败，而不是假装连接远端: {error}"
    );
}

#[tokio::test]
async fn site_check_in_endpoint_runs_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/sites/{}/check-in", site["id"].as_str().unwrap()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]["message"].as_str().unwrap_or_default().contains("Cookie"),
        "占位站点不得报告签到成功: {body}"
    );
}

#[tokio::test]
async fn site_login_endpoint_runs_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/sites/{}/login", site["id"].as_str().unwrap()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]["message"].as_str().unwrap_or_default().contains("Cookie"),
        "空登录请求不得伪造会话: {body}"
    );
}

#[tokio::test]
async fn site_actions_reject_unknown_site() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/sites/00000000-0000-0000-0000-0000000000ff/check-in",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
