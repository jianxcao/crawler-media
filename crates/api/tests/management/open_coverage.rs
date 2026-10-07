use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn open_tv_coverage_tracks_observed_and_next_episode_without_completing() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let mut payload = subscribe_payload("search");
    payload["media"] = json!({"kind":"tv", "title":"The Office"});
    payload["coverage"] = json!({"kind":"tv", "season":2, "episode_from":1});
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            payload,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_data(created).await["id"].as_str().unwrap().to_string();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let subscribe_id = id.parse().unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        Some(2),
        Some(1),
        subscribe::QualityFact {
            score: 80,
            path: Some("/library/office-s02e01.mkv".into()),
        },
    );
    store.save_subscribe_facts(subscribe_id, &facts).unwrap();
    store
        .merge_pending(
            subscribe_id,
            &[(
                80,
                api::store::PendingDownload {
                    torrent: domain::Torrent {
                        site_id: domain::SiteId::new(),
                        title: "The.Office.S02E04.1080p".into(),
                        enclosure: "https://pt.example/office-4".into(),
                        size_bytes: None,
                        seeders: None,
                        free: false,
                        hr: false,
                        imdb_id: None,
                        id: None,
                        leechers: None,
                        snatched: None,
                        upload_time: None,
                        detail_url: None,
                        category: None,
                        poster_url: None,
                    },
                    release_override: None,
                    downloader_id: None,
                    submitted_at: None,
                },
            )],
        )
        .unwrap();
    let mut searched = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211, 241, 271] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        searched = store
            .load_wanted_history(subscribe_id)
            .unwrap()
            .get(&(Some(2), Some(5)))
            .is_some_and(|row| row.search_attempts > 0);
        if searched {
            break;
        }
    }
    assert!(
        searched,
        "the next episode should remain a wanted search target"
    );
    assert!(
        store
            .get_setting(&format!("subscribe.completed:{id}"))
            .unwrap()
            .is_none()
    );
    let detail = json_body(
        app.oneshot(request(
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
        wanted
            .iter()
            .map(|unit| unit["episode_number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(wanted[3]["status"], "grabbed");
    assert_eq!(wanted[4]["status"], "wanted");
}

#[tokio::test]
async fn open_tv_coverage_progress_total_is_at_least_one() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "tv", "title": "The Office" },
                "coverage": { "kind": "tv", "season": 2, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let detail = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let total = detail["data"]["progress"]["total"].as_i64().unwrap();
    let missing = detail["data"]["progress"]["missing"].as_i64().unwrap();
    assert!(
        total >= 1,
        "open TV coverage must not report total=0: {detail}"
    );
    assert!(missing >= 0, "missing must be non-negative: {detail}");
}
