use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use super::subscription_depth::{authed_app, seed_pending_torrent};
async fn poll_for_activity_type(app: &axum::Router, id: &str, target: &str) -> bool {
    for _ in 0..10 {
        let activities = json_body(
            app.clone()
                .oneshot(request(
                    "GET",
                    &format!("/api/v1/subscriptions/{id}/activities"),
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap(),
        )
        .await;
        let items = activities["data"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let types: Vec<_> = items
            .iter()
            .filter_map(|item| item["type"].as_str())
            .collect();
        if types.contains(&target) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn activities_report_search_rounds_and_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let app = router(state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    create_site(&app).await;
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    for now in [1, 31, 61, 91, 121, 151] {
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
    }
    assert!(
        poll_for_activity_type(&app, &id, "searched").await,
        "应有 searched 活动"
    );
}

// ---------------------------------------------------------------------------
// U1 wanted 工单：单元状态（wanted/grabbed/imported）现算 + 履历（attempts/
// grab/reject/imported_at）持久化。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wanted_list_starts_wanted_without_activity() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let wanted = detail["data"]["wanted"].as_array().unwrap();
    assert_eq!(wanted.len(), 1, "movie 订阅应有一个工单单元");
    assert_eq!(wanted[0]["status"], "wanted");
    assert_eq!(wanted[0]["season_number"], 0);
    assert_eq!(wanted[0]["episode_number"], 0);
    assert_eq!(wanted[0]["search_attempts"], 0);
    assert_eq!(
        wanted[0]["upgrade"],
        Value::Null,
        "未入库单元 upgrade 应为 null"
    );
}

async fn create_matrix_sub(app: &axum::Router) -> String {
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    created["data"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn wanted_marked_grabbed_after_search_round() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let id = create_matrix_sub(&app).await;
    for now in [1, 31, 61, 91, 121, 151] {
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        if downloader.added().len() == 1 {
            break;
        }
    }
    assert_eq!(downloader.added().len(), 1, "搜索 job 应命中 Matrix 种子");

    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let wanted = detail["data"]["wanted"].as_array().unwrap();
    assert_eq!(wanted[0]["status"], "grabbed");
    assert!(wanted[0]["grab_title"].as_str().unwrap().contains("Matrix"));
    assert!(wanted[0]["search_attempts"].as_i64().unwrap() >= 1);
    assert!(wanted[0]["grabbed_at"].is_string());
    assert_eq!(wanted[0]["upgrade"]["active"], false);
}

async fn poll_for_wanted_status(
    app: &axum::Router,
    id: &str,
    downloader: &MemoryDownloader,
) -> bool {
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let _ = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await;
        if downloader.added().len() == 1 {
            let detail = json_body(
                app.clone()
                    .oneshot(request(
                        "GET",
                        &format!("/api/v1/subscriptions/{id}"),
                        Some("management-secret"),
                        Value::Null,
                    ))
                    .await
                    .unwrap(),
            )
            .await;
            if detail["data"]["wanted"][0]["status"] == "imported" {
                return true;
            }
        }
    }
    false
}

#[tokio::test]
async fn wanted_marked_imported_after_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&source, b"movie-bytes").unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    assert!(
        poll_for_wanted_status(&app, &id, &downloader).await,
        "Transfer job 后工单应标记 imported"
    );
    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let wanted = detail["data"]["wanted"].as_array().unwrap();
    assert_eq!(wanted[0]["status"], "imported");
    assert!(wanted[0]["imported_at"].is_string());
    assert!(
        wanted[0]["upgrade"]["current_label"].as_str().is_some(),
        "imported 单元应有当前档位标签"
    );
}

async fn create_rule_set_1080p(app: &axum::Router) -> String {
    let rule = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "only-1080p",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 100 }],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    rule["data"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn wanted_records_reject_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let filter_id = create_rule_set_1080p(&app).await;
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                    "filter_id": filter_id,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    for now in [1, 31, 61, 91, 121, 151] {
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
    }
    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let wanted = detail["data"]["wanted"].as_array().unwrap();
    assert_eq!(
        wanted[0]["status"], "wanted",
        "被规则组拒绝不应改变 wanted 状态"
    );
    let reason = wanted[0]["last_reject_reason"].as_str().unwrap_or("");
    assert!(reason.contains("未匹配规则组"), "应记录拒绝原因: {reason}");
    assert!(wanted[0]["search_attempts"].as_i64().unwrap() >= 1);
}
