use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, request, state, subscribe_payload};

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

fn create_body(legacy: bool, coverage: Value, full_season_pack: bool) -> (&'static str, Value) {
    if legacy {
        let mut body = subscribe_payload("search");
        body["media"]["kind"] = json!("tv");
        body["coverage"] = coverage;
        body["full_season_pack"] = json!(full_season_pack);
        ("/api/v1/subscriptions", body)
    } else {
        (
            "/api/v1/subscriptions",
            json!({
                "media": { "kind": "tv", "title": "Coverage Validation" },
                "coverage": coverage,
                "full_season_pack": full_season_pack
            }),
        )
    }
}

#[tokio::test]
async fn both_create_apis_reject_invalid_tv_coverage_before_writes() {
    let invalid_coverage = [
        json!({ "kind": "tv", "season": 1, "episode_from": 5, "episode_to": 4 }),
        json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1001 }),
        json!({ "kind": "tv", "season": 0, "episode_from": 1, "episode_to": 2 }),
        json!({ "kind": "tv", "season": 1, "episode_from": 0, "episode_to": 2 }),
    ];

    for legacy in [true, false] {
        for coverage in invalid_coverage.clone() {
            assert_rejected_without_writes(legacy, coverage, false).await;
        }
        assert_rejected_without_writes(
            legacy,
            json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": null }),
            true,
        )
        .await;
    }
}

async fn assert_rejected_without_writes(legacy: bool, coverage: Value, full_season_pack: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let before_filters = rows(&tmp, "app.db", "filters");
    let (uri, body) = create_body(legacy, coverage, full_season_pack);
    let response = app
        .oneshot(request("POST", uri, Some("management-secret"), body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    assert_eq!(rows(&tmp, "app.db", "media"), 0, "{uri}");
    assert_eq!(rows(&tmp, "app.db", "filters"), before_filters, "{uri}");
    assert_eq!(rows(&tmp, "subscribe.db", "subscribes"), 0, "{uri}");
}

#[tokio::test]
async fn patch_rejects_full_season_pack_for_open_tv_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "tv", "title": "Open Coverage" },
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": null },
                "full_season_pack": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &axum::body::to_bytes(created.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let id = created["data"]["id"].as_str().unwrap();

    let response = app
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "full_season_pack": true }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_rejects_malformed_policy_references() {
    for field in ["downloader_id", "wash_cut_filter_id"] {
        for value in ["not-a-uuid", "00000000-0000-0000-0000-000000000001"] {
            let tmp = tempfile::tempdir().unwrap();
            let app = app(&tmp);
            let mut body = json!({
                "media": { "kind": "movie", "title": "Bad Policy Reference" },
                "coverage": { "kind": "movie" },
                "wash_cut": true
            });
            body[field] = json!(value);
            let response = app
                .oneshot(request(
                    "POST",
                    "/api/v1/subscriptions",
                    Some("management-secret"),
                    body,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{field}={value}"
            );
            assert_eq!(
                rows(&tmp, "subscribe.db", "subscribes"),
                0,
                "{field}={value}"
            );
        }
    }
}

#[tokio::test]
async fn patch_rejects_malformed_downloader_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "Patch Policy Reference" },
                "coverage": { "kind": "movie" }
            }),
        ))
        .await
        .unwrap();
    let created: Value = serde_json::from_slice(
        &axum::body::to_bytes(created.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let id = created["data"]["id"].as_str().unwrap();

    let response = app
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "downloader_id": "not-a-uuid" }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
