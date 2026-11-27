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

#[tokio::test]
async fn list_tasks_matches_untagged_http_snapshot_via_persisted_info_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    );
    let app = router(api_state.clone());
    let sub = create_api_subscribe(&app, "The Matrix", "management-secret").await;
    let sid: domain::SubscribeId = sub["id"].as_str().unwrap().parse().unwrap();
    let enclosure = "https://pt.example/download.php?id=91";
    let hash = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let torrent = domain::Torrent {
        site_id: domain::SiteId::new(),
        title: "The.Matrix.1999.1080p".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(100),
        seeders: Some(5),
        free: false,
        hr: None,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
        free_2x: false,
        free_until: None,
    };
    let store = api_state.store();
    store
        .record_pending_submission(
            sid,
            100,
            &api::store::PendingDownload {
                torrent,
                release_override: None,
                downloader_id: None,
                submitted_at: Some(crate::common::next_playback_sequence()),
            },
        )
        .unwrap();
    store.set_pending_info_hash(sid, enclosure, hash).unwrap();
    downloader.push_snapshot(downloader::TaskSnapshot {
        tag: String::new(),
        name: "The.Matrix.1999.1080p".into(),
        progress: 0.55,
        state: "downloading".into(),
        size_bytes: 100,
        downloaded_bytes: 55,
        uploaded_bytes: 0,
        download_speed: 10,
        upload_speed: 0,
        info_hash: hash.into(),
        owned_tags: vec![],
        ..Default::default()
    });

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
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "downloading");
    assert_eq!(rows[0]["info_hash"], hash);
    assert_eq!(rows[0]["progress"], 0.55);
}
