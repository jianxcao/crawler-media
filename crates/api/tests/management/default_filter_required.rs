use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::json;
use tower::ServiceExt;

use super::common::{Fixtures, json_body, request, state};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

fn rows(tmp: &tempfile::TempDir, db: &str, table: &str) -> i64 {
    Connection::open(tmp.path().join("data").join(db))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn missing_default_filter_is_rejected_before_writes_but_explicit_filter_works() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let filter = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "explicit-filter",
                "atoms": [{ "kind": "title_match", "value": "Filter Test", "priority": 10 }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(filter.status(), StatusCode::CREATED);
    let filter = super::common::json_data(filter).await;

    Connection::open(tmp.path().join("data/app.db"))
        .unwrap()
        .execute("DELETE FROM settings WHERE key = 'default_filter_id'", [])
        .unwrap();
    let before_media = rows(&tmp, "app.db", "media");
    let before_filters = rows(&tmp, "app.db", "filters");
    let before_subscribes = rows(&tmp, "subscribe.db", "subscribes");
    let before_jobs = rows(&tmp, "jobs.db", "job_defs");
    let body = json!({
        "media": { "kind": "movie", "title": "Filter Test Movie" },
        "coverage": { "kind": "movie" }
    });
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body.clone(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(rows(&tmp, "app.db", "media"), before_media);
    assert_eq!(rows(&tmp, "app.db", "filters"), before_filters);
    assert_eq!(rows(&tmp, "subscribe.db", "subscribes"), before_subscribes);
    assert_eq!(rows(&tmp, "jobs.db", "job_defs"), before_jobs);

    Connection::open(tmp.path().join("data/app.db"))
        .unwrap()
        .execute(
            "INSERT INTO settings(key, value) VALUES ('default_filter_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params!["00000000-0000-0000-0000-000000000000"],
        )
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body.clone(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(rows(&tmp, "app.db", "media"), before_media);
    assert_eq!(rows(&tmp, "app.db", "filters"), before_filters);
    assert_eq!(rows(&tmp, "subscribe.db", "subscribes"), before_subscribes);
    assert_eq!(rows(&tmp, "jobs.db", "job_defs"), before_jobs);

    let mut explicit_body = body;
    explicit_body["filter_id"] = filter["id"].clone();
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            explicit_body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(json_body(response).await["data"]["filter_id"], filter["id"]);
    assert_eq!(
        rows(&tmp, "subscribe.db", "subscribes"),
        before_subscribes + 1
    );
}
