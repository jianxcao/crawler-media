use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn admit_chosen_torrent_adds_to_downloader_and_downloads() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;

    let searched = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(searched.status(), StatusCode::OK);
    let body = json_data(searched).await;
    let enclosure = body["items"][0]["enclosure"].as_str().unwrap();

    let admitted = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/search/admit",
            Some("management-secret"),
            json!({
                "subscribe_id": subscribe["id"],
                "enclosure": enclosure
            }),
        ))
        .await
        .unwrap();
    assert_eq!(admitted.status(), StatusCode::OK);
    let admitted_data = json_data(admitted).await;
    assert_eq!(
        admitted_data["title"],
        "The.Matrix.1999.2160p.BluRay.x265-GROUP"
    );
    assert_eq!(downloader.added().len(), 1);
    assert_eq!(
        downloader.added()[0].title,
        "The.Matrix.1999.2160p.BluRay.x265-GROUP"
    );

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/downloaders/tasks",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let items = rows["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0]["id"].as_str().unwrap().contains(enclosure));
    assert_eq!(items[0]["subscriptions"][0]["id"], subscribe["id"]);
    assert_eq!(items[0]["media_title"], "The Matrix");
}

#[tokio::test]
async fn admit_rejects_non_existent_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let rejected = app
        .oneshot(request(
            "POST",
            "/api/v1/search/admit",
            Some("management-secret"),
            json!({
                "subscribe_id": subscribe["id"],
                "enclosure": "https://pt.example/download.php?id=999"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let error = json_error(rejected).await;
    assert_eq!(error["code"], "torrent.missing");
    assert!(downloader.added().is_empty());
}

#[tokio::test]
async fn admit_rejects_torrent_that_fails_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;

    let filter_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "exclude-2160p",
                "atoms": [
                    { "kind": "resolution", "value": "2160p", "priority": 0, "exclude": true }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(filter_res.status(), StatusCode::CREATED);
    let filter_data = json_data(filter_res).await;

    let put_def = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter_data["id"] }),
        ))
        .await
        .unwrap();
    assert_eq!(put_def.status(), StatusCode::OK);

    let sub_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "The Matrix" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_data["id"]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(sub_res.status(), StatusCode::CREATED);
    let subscribe = json_data(sub_res).await;

    let rejected = app
        .oneshot(request(
            "POST",
            "/api/v1/search/admit",
            Some("management-secret"),
            json!({
                "subscribe_id": subscribe["id"],
                "enclosure": "https://pt.example/download.php?id=1&passkey=abc"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let error = json_error(rejected).await;
    assert_eq!(error["code"], "subscribe.rejected");
    assert!(downloader.added().is_empty());
}

#[tokio::test]
async fn admit_uses_the_subscribe_filter_not_the_global_default() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;

    let filter_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": "subscribe-exclude-2160p",
                "atoms": [
                    { "kind": "resolution", "value": "2160p", "priority": 0, "exclude": true }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(filter_res.status(), StatusCode::CREATED);
    let filter_data = json_data(filter_res).await;

    let sub_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "The Matrix" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "filter_id": filter_data["id"]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(sub_res.status(), StatusCode::CREATED);
    let subscribe = json_data(sub_res).await;

    let rejected = app
        .oneshot(request(
            "POST",
            "/api/v1/search/admit",
            Some("management-secret"),
            json!({
                "subscribe_id": subscribe["id"],
                "enclosure": "https://pt.example/download.php?id=1&passkey=abc"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let error = json_error(rejected).await;
    assert_eq!(error["code"], "subscribe.rejected");
    assert!(downloader.added().is_empty());
}

#[tokio::test]
async fn admit_with_release_override_persists_corrected_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let enclosure = "https://pt.example/download.php?id=1&passkey=abc";

    let admitted = app
        .oneshot(request(
            "POST",
            "/api/v1/search/admit",
            Some("management-secret"),
            json!({
                "subscribe_id": subscribe["id"],
                "enclosure": enclosure,
                "release": {
                    "title": "The Matrix 4K Remux",
                    "year": 1999,
                    "resolution": "2160p",
                    "source": "UHD BluRay"
                }
            }),
        ))
        .await
        .unwrap();
    let status = admitted.status();
    let dbg = json_body(admitted).await;
    assert_eq!(status, StatusCode::OK, "admit body: {dbg}");

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let id = domain::SubscribeId::from_str(subscribe["id"].as_str().unwrap()).unwrap();
    let pending = store.load_pending(id).unwrap();
    assert_eq!(pending.len(), 1);
    let release = pending[0].1.release_override.as_ref().unwrap();
    assert_eq!(release.title, "The Matrix 4K Remux");
    assert_eq!(release.resolution.as_deref(), Some("2160p"));
    assert_eq!(release.source.as_deref(), Some("UHD BluRay"));
    assert_eq!(release.year, Some(1999));
}

#[tokio::test]
async fn admit_without_override_keeps_parsed_release() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let admitted = app
        .oneshot(request(
            "POST",
            "/api/v1/search/admit",
            Some("management-secret"),
            json!({
                "subscribe_id": subscribe["id"],
                "enclosure": "https://pt.example/download.php?id=1&passkey=abc"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(admitted.status(), StatusCode::OK);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let id = domain::SubscribeId::from_str(subscribe["id"].as_str().unwrap()).unwrap();
    let pending = store.load_pending(id).unwrap();
    assert!(pending[0].1.release_override.is_none());
}
