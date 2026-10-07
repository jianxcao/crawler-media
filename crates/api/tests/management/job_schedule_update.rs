use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn test_update_job_schedule_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));

    // 先取得系统任务列表中的一个任务 id（例如 Transfer）
    let list_res = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/jobs?scope=system",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(list_res.status(), StatusCode::OK);

    let json = json_body(list_res).await;
    let jobs = json["data"].as_array().unwrap();
    let transfer_job = jobs
        .iter()
        .find(|j| j["name"] == "Transfer")
        .expect("Transfer job must exist");
    let job_id = transfer_job["id"].as_str().unwrap();

    // 请求更新周期为 300 秒 (5分钟)
    let update_res = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/jobs/{job_id}/schedule"),
            Some("management-secret"),
            json!({ "interval_secs": 300 }),
        ))
        .await
        .unwrap();
    assert_eq!(update_res.status(), StatusCode::OK);

    // 重新获取该任务，验证 schedule 已变为 300 秒
    let get_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/jobs/{job_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::OK);

    let job_json = json_body(get_res).await;
    assert_eq!(job_json["data"]["schedule"]["interval_secs"], 300);
}
