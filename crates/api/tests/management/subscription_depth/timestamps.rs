//! Timestamps, real progress and the removal preview of a Subscription.

use super::support::*;

#[tokio::test]
async fn subscription_has_real_timestamps_and_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = app
        .clone()
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
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let data = &created["data"];
    let created_at = data["created_at"].as_str().unwrap();
    assert_ne!(created_at, "1970-01-01T00:00:00Z");
    assert!(
        created_at.starts_with("20"),
        "created_at 应为真实时间: {created_at}"
    );
    let id = data["id"].as_str().unwrap();

    // 初始 progress：movie total=1, imported=0, grabbing=0。
    assert_eq!(data["progress"]["total"], 1);
    assert_eq!(data["progress"]["imported"], 0);
    assert_eq!(data["progress"]["grabbing"], 0);

    // PATCH 后 updated_at 前进。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "tracking_state": "paused" }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = json_body(patched).await;
    let updated_at = patched["data"]["updated_at"].as_str().unwrap();
    assert!(
        updated_at > created_at,
        "updated_at 应晚于 created_at: {updated_at} vs {created_at}"
    );
}

async fn assert_progress_and_preview(app: &axum::Router, id: &str) {
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
    assert_eq!(detail["data"]["progress"]["grabbing"], 1);
    assert_eq!(detail["data"]["progress"]["downloaded"], 1);
    assert_eq!(detail["data"]["progress"]["missing"], 0);

    let preview = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}/removal-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(preview["data"]["torrent_count"], 1);
    assert!(
        preview["data"]["torrent_titles"][0]
            .as_str()
            .unwrap()
            .contains("Matrix")
    );

    let arrivals = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/subscriptions/today-arrivals",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let items = arrivals["data"].as_array().unwrap();
    assert!(items.iter().any(|item| item["status"] == "grabbed"));
}

#[tokio::test]
async fn progress_and_removal_preview_count_pending_torrents() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
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
    assert_eq!(
        downloader.added().len(),
        1,
        "搜索 job 应把 Matrix 种子投给下载器"
    );

    assert_progress_and_preview(&app, &id).await;
}
