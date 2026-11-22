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
    assert_eq!(
        preview["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "library_bytes",
            "library_file_count",
            "torrent_count",
            "torrent_count_inferred",
            "torrent_titles"
        ]
    );
    assert_eq!(preview["data"]["torrent_count_inferred"], false);
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

#[tokio::test]
async fn subscription_timestamp_milliseconds_are_exact_and_errors_are_explicit() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({"media":{"kind":"movie","title":"Clock"},"coverage":{"kind":"movie"}}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    db.execute(
        "INSERT INTO subscribe_facts(subscribe_id,season,episode,score,path) VALUES(?1,-1,-1,1,'/fixture.mkv')",
        [id],
    )
    .unwrap();
    for (ms, expected) in [
        (-1_i64, "1969-12-31T23:59:59.999Z"),
        (-62_167_219_200_000, "0000-01-01T00:00:00.000Z"),
        (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
        (1_800_000_000_123, "2027-01-15T08:00:00.123Z"),
    ] {
        db.execute(
            "UPDATE subscribes SET created_at=?2,updated_at=?2 WHERE id=?1",
            rusqlite::params![id, ms],
        )
        .unwrap();
        for path in [
            format!("/api/v1/subscriptions/{id}"),
            "/api/v1/subscriptions".into(),
            format!("/api/v1/subscriptions/{id}/activities"),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    "GET",
                    &path,
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            if path.ends_with("activities") {
                assert_eq!(
                    body["data"]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["kind"] == "subscribe_facts")
                        .unwrap()["started_at"],
                    expected
                );
            } else if path.ends_with(id) {
                assert_eq!(body["data"]["created_at"], expected);
                assert_eq!(body["data"]["updated_at"], expected);
            } else {
                assert_eq!(body["data"][0]["created_at"], expected, "{body}");
            }
        }
    }
    for value in [
        serde_json::json!(i64::MAX),
        serde_json::json!(-62_167_219_200_001_i64),
        serde_json::json!(253_402_300_800_000_i64),
        serde_json::json!("broken"),
    ] {
        let value = match value {
            Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
            Value::String(s) => rusqlite::types::Value::Text(s),
            _ => unreachable!(),
        };
        db.execute(
            "UPDATE subscribes SET created_at=?2 WHERE id=?1",
            rusqlite::params![id, value],
        )
        .unwrap();
        for path in [
            format!("/api/v1/subscriptions/{id}"),
            "/api/v1/subscriptions".into(),
            format!("/api/v1/subscriptions/{id}/activities"),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    "GET",
                    &path,
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn create_and_patch_report_timestamp_read_errors_after_business_write() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let db = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER corrupt_creation_time AFTER INSERT ON subscribes BEGIN UPDATE subscribes SET created_at='broken' WHERE id=NEW.id; END;").unwrap();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({"media":{"kind":"movie","title":"Invalid clock"},"coverage":{"kind":"movie"}}),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let id: String = db
        .query_row("SELECT id FROM subscribes", [], |r| r.get(0))
        .unwrap();
    db.execute_batch("DROP TRIGGER corrupt_creation_time;")
        .unwrap();
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({"tracking_state":"paused"}),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let state: String = db
        .query_row(
            "SELECT tracking_state FROM subscribes WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        state, "paused",
        "render errors do not roll back committed business writes"
    );
}

#[tokio::test]
async fn removal_preview_marks_matching_downloader_counts_as_inferred() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    downloader.push_snapshot(downloader::TaskSnapshot {
        tag: downloader::TASK_TAG.into(),
        name: "The Matrix.2026.1080p".into(),
        ..Default::default()
    });
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    ));
    let created = json_body(app.clone().oneshot(request("POST", "/api/v1/subscriptions",
        Some("management-secret"), json!({
            "media": { "kind": "movie", "title": "The Matrix" }, "coverage": { "kind": "movie" },
        }))).await.unwrap()).await;
    let id = created["data"]["id"].as_str().unwrap();
    let preview = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}/removal-preview"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        preview["data"],
        json!({
            "library_file_count": 0, "library_bytes": 0, "torrent_count": 1,
            "torrent_count_inferred": true, "torrent_titles": ["The Matrix.2026.1080p"],
        })
    );
}

#[tokio::test]
async fn removal_preview_counts_imported_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let empty_default = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        empty_default,
    ));

    let sub_a = json_body(
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
    let id_a = sub_a["data"]["id"].as_str().unwrap();
    seed_pending_torrent(
        &tmp,
        id_a,
        "The.Matrix.1999.2160p",
        "https://pt.example/imported",
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .mark_pending_imported(
            id_a.parse().unwrap(),
            &["https://pt.example/imported".into()],
        )
        .unwrap();
    let preview_a = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id_a}/removal-preview"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(preview_a["data"]["torrent_count"], 1);
    assert_eq!(preview_a["data"]["torrent_count_inferred"], false);
    assert_eq!(
        preview_a["data"]["torrent_titles"],
        json!(["The.Matrix.1999.2160p"])
    );
}

#[tokio::test]
async fn removal_preview_uses_subscribe_downloader() {
    let tmp = tempfile::tempdir().unwrap();
    let empty_default = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        empty_default,
    ));
    let tr_server = super::super::lifecycle::FakeServer::start(|request, stream| {
        let body = if request.raw.contains("torrent-get") {
            r#"{"result":"success","arguments":{"torrents":[{"id":1,"name":"The.Matrix.1999.1080p","hashString":"0123456789abcdef0123456789abcdef01234567","sizeWhenDone":100,"labels":["crawler-media"]}]}}"#
        } else {
            r#"{"result":"success","arguments":{"rpc-version":16}}"#
        };
        super::super::lifecycle::respond(
            stream,
            "200 OK",
            "X-Transmission-Session-Id: test\r\n",
            body.as_bytes(),
        );
    });
    let tr_dl = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/downloaders",
                Some("management-secret"),
                json!({
                    "name": "tr-secondary",
                    "kind": "transmission",
                    "url": format!("{}/transmission/rpc", tr_server.url()),
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let dl_id = tr_dl["id"].as_str().unwrap();
    let sub_b = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                    "downloader_id": dl_id,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id_b = sub_b["data"]["id"].as_str().unwrap();
    let preview_b = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id_b}/removal-preview"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(preview_b["data"]["torrent_count"], 1);
    assert_eq!(preview_b["data"]["torrent_count_inferred"], true);
    assert_eq!(
        preview_b["data"]["torrent_titles"],
        json!(["The.Matrix.1999.1080p"])
    );
}
