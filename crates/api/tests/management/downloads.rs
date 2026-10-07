use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn downloads_lists_pending_torrents_after_search_job() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    create_subscribe(&app, "search").await;

    for now in [1, 31, 61, 91, 121, 151] {
        let tick = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(tick.status(), StatusCode::OK);
        if downloader.added().len() == 1 {
            break;
        }
    }
    assert_eq!(downloader.added().len(), 1);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/downloaders/tasks",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let data = json_data(listed).await;
    let rows = data["items"].as_array().unwrap();
    let row = &rows[0];
    assert!(row["name"].as_str().unwrap().contains("Matrix"));
    assert_eq!(row["media_title"], "The Matrix");
    assert!(row["subscriptions"][0]["id"].as_str().is_some());
}
