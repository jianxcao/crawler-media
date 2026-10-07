use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn list_subscribes_includes_media_title_and_tv_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let mut body = subscribe_payload("search");
    body["media"] = json!({
        "kind": "tv",
        "title": "The Long Watch",
        "tmdb_id": "100"
    });
    body["coverage"] = json!({
        "kind": "tv",
        "season": 1,
        "episode_from": 1,
        "episode_to": 8
    });
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["media"]["title"] == "The Long Watch")
        .expect("listed Subscribe");
    assert_eq!(row["media"]["kind"], "tv");
    assert_eq!(row["coverage"]["kind"], "tv");
    assert_eq!(row["coverage"]["season"], 1);
    assert_eq!(row["coverage"]["episode_from"], 1);
    assert_eq!(row["coverage"]["episode_to"], 8);
    assert_eq!(row["fetch_mode"], "search");
}

#[tokio::test]
async fn list_subscribes_reports_owned_and_missing_tv_episodes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let mut body = subscribe_payload("search");
    body["media"] = json!({
        "kind": "tv",
        "title": "The Long Watch",
        "tmdb_id": "100"
    });
    body["coverage"] = json!({
        "kind": "tv",
        "season": 1,
        "episode_from": 1,
        "episode_to": 4
    });
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    let sub_id = created["id"].as_str().unwrap();
    seed_two_episodes(&tmp, sub_id);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["media"]["title"] == "The Long Watch")
        .expect("listed Subscribe");
    assert_eq!(row["progress"]["imported"], 2);
    assert_eq!(row["progress"]["total"], 4);
    assert_eq!(row["progress"]["missing"], 2);
}

fn seed_two_episodes(tmp: &tempfile::TempDir, subscribe_id: &str) {
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sub_id = domain::SubscribeId::from_str(subscribe_id).unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        subscribe::QualityFact {
            score: 100,
            path: Some("/tmp/e1.mkv".into()),
        },
    );
    facts.upsert(
        Some(1),
        Some(2),
        subscribe::QualityFact {
            score: 100,
            path: Some("/tmp/e2.mkv".into()),
        },
    );
    store.save_subscribe_facts(sub_id, &facts).unwrap();
}

#[tokio::test]
async fn list_subscribes_still_returns_movie_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    create_subscribe(&app, "rss").await;
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media"]["title"], "The Matrix");
    assert_eq!(row["media"]["kind"], "movie");
    assert_eq!(row["coverage"]["kind"], "movie");
    assert_eq!(row["fetch_mode"], "rss");
}

async fn tick_all(app: &axum::Router, timestamps: &[u64]) {
    for now in timestamps {
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
    }
}

#[tokio::test]
async fn paused_subscribe_does_not_hit_indexer() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher.clone(), downloader));
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
                    "fetch_mode": "search"
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().expect("subscribe id");

    let patched = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                json!({ "tracking_state": "paused" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(patched["data"]["tracking_state"], "paused");

    tick_all(&app, &[1, 31, 61, 91, 121, 151, 181, 211, 1801, 1831]).await;
    assert!(
        fetcher.requests.lock().is_empty(),
        "paused Subscribe must not search Sites: {:?}",
        fetcher.requests.lock()
    );
}

#[tokio::test]
async fn create_subscribe_seeds_search_job_at_custom_interval() {
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
                "media": { "kind": "movie", "title": "The Matrix" },
                "coverage": { "kind": "movie" },
                "search_interval_secs": 3600
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["data"]["search_interval_secs"], 3600);
    let id = created["data"]["id"].as_str().unwrap();

    let jobs = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/jobs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let defs = jobs["data"].as_array().expect("jobs list");
    let search = defs
        .iter()
        .find(|row| {
            row["kind"] == "subscribe_search" && row["name"].as_str().unwrap_or("").contains(id)
        })
        .expect("search JobDef");
    assert_eq!(search["schedule"]["interval_secs"], 3600);
}

async fn create_simple_subscribe(app: &axum::Router) -> (String, i64) {
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" }
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    (
        created["data"]["id"].as_str().unwrap().to_string(),
        created["data"]["search_interval_secs"].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn patch_interval_updates_job_def() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let (id, interval) = create_simple_subscribe(&app).await;
    assert_eq!(interval, 1800);

    let patched = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                json!({ "search_interval_secs": 7200 }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(patched["data"]["search_interval_secs"], 7200);

    let jobs = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/jobs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let defs = jobs["data"].as_array().expect("jobs list");
    let search = defs
        .iter()
        .find(|row| {
            row["kind"] == "subscribe_search" && row["name"].as_str().unwrap_or("").contains(&id)
        })
        .expect("search JobDef");
    assert_eq!(search["schedule"]["interval_secs"], 7200);
}

#[tokio::test]
async fn interval_out_of_range_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "The Matrix" },
                "coverage": { "kind": "movie" },
                "search_interval_secs": 60
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn create_movie_and_tv_libraries(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> (String, String, String) {
    let lib1 = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "电影库1",
                    "kind": "movie",
                    "root_paths": [tmp.path().join("lib1").display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib2 = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "电影库2",
                    "kind": "movie",
                    "root_paths": [tmp.path().join("lib2").display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib_tv = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "剧集库",
                    "kind": "tv",
                    "root_paths": [tmp.path().join("lib_tv").display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    (
        lib1["data"]["id"].as_str().unwrap().to_string(),
        lib2["data"]["id"].as_str().unwrap().to_string(),
        lib_tv["data"]["id"].as_str().unwrap().to_string(),
    )
}

async fn assert_subscription_library_patch(
    app: &axum::Router,
    sub_id: &str,
    lib2_id: &str,
    lib_tv_id: &str,
) {
    let patched2 = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{sub_id}"),
                Some("management-secret"),
                json!({ "library_id": lib2_id }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(patched2["data"]["library_id"], lib2_id);

    let reject_mismatch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "library_id": lib_tv_id }),
        ))
        .await
        .unwrap();
    assert_eq!(reject_mismatch.status(), StatusCode::BAD_REQUEST);

    let cleared = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{sub_id}"),
                Some("management-secret"),
                json!({ "library_id": Value::Null }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cleared["data"]["library_id"], Value::Null);
}

#[tokio::test]
async fn patch_subscription_library_id_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let (lib1_id, lib2_id, lib_tv_id) = create_movie_and_tv_libraries(&app, &tmp).await;

    // 创建电影订阅，初始绑定 lib1
    let sub = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                    "library_id": lib1_id,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let sub_id = sub["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(sub["data"]["library_id"], lib1_id);

    assert_subscription_library_patch(&app, &sub_id, &lib2_id, &lib_tv_id).await;
}
