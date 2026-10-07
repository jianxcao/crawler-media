use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::to_bytes;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{json_body, request};

struct CapturingFetcher {
    bodies: Mutex<Vec<Value>>,
}

impl Fetcher for CapturingFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.bodies
            .lock()
            .push(serde_json::from_str(request.body.as_deref().unwrap()).unwrap());
        Ok(include_str!("../../../indexer/tests/fixtures/mteam.json").into())
    }
}

#[tokio::test]
async fn manual_search_applies_categories_and_private_history_for_json_and_sse() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(CapturingFetcher {
        bodies: Mutex::new(Vec::new()),
    });
    let app = router(
        ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher.clone(),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({"name":"M-Team", "url":"https://kp.m-team.cc/",
            "profile_id":"mteam", "api_key":"test-key", "enabled":true}),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let private = app.clone().oneshot(request("GET",
        "/api/v1/search/torrents?keyword=dune&categories=movie&save_history=true&skip_history=true",
        Some("management-secret"), Value::Null)).await.unwrap();
    assert_eq!(private.status(), StatusCode::OK);
    assert_eq!(
        json_body(private).await["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fetcher.bodies.lock()[0]["categories"],
        json!([401, 419, 420, 421, 439])
    );
    let history = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert!(
        json_body(history).await["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let normal = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents?keyword=dune&save_history=true",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(normal.status(), StatusCode::OK);
    assert!(fetcher.bodies.lock()[1].get("categories").is_none());

    let streamed = app.clone().oneshot(request("GET",
        "/api/v1/search/torrents/stream?keyword=dune&categories=tv&save_history=true&skip_history=true",
        Some("management-secret"), Value::Null)).await.unwrap();
    assert_eq!(streamed.status(), StatusCode::OK);
    let events = to_bytes(streamed.into_body(), 64 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&events).contains("event: done"));
    assert_eq!(
        fetcher.bodies.lock()[2]["categories"],
        json!([403, 402, 435, 438, 407])
    );
    let history = app
        .oneshot(request(
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        json_body(history).await["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn empty_keyword_browse_works_for_json_and_stream_without_history() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(CapturingFetcher {
        bodies: Mutex::new(Vec::new()),
    });
    let app = router(
        ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher.clone(),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({"name":"M-Team", "url":"https://kp.m-team.cc/",
        "profile_id":"mteam", "api_key":"test-key", "enabled":true}),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let json_result = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents?categories=movie&save_history=true",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_result.status(), StatusCode::OK);
    assert_eq!(json_body(json_result).await["data"]["keyword"], "");
    assert_eq!(
        fetcher.bodies.lock()[0]["categories"],
        json!([401, 419, 420, 421, 439])
    );

    let streamed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents/stream?categories=tv",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(streamed.status(), StatusCode::OK);
    let events = to_bytes(streamed.into_body(), 64 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&events).contains("event: done"));
    assert_eq!(
        fetcher.bodies.lock()[1]["categories"],
        json!([403, 402, 435, 438, 407])
    );

    let history = app
        .oneshot(request(
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert!(
        json_body(history).await["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
