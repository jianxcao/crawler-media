use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::SubscribeId;
use domain::{QualityFact, SubscribeFacts};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, create_api_subscribe, json_body, request, state};

#[tokio::test]
async fn missing_search_rejects_paused_and_complete_subscribes_and_resets_open_gap_cooldowns() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let active = create_api_subscribe(&app, "Missing Movie", "management-secret").await;
    let active_id = active["id"].as_str().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let subscribe_id = SubscribeId::from_str(active_id).unwrap();
    store
        .touch_wanted_searches(subscribe_id, &[(None, None)], 1)
        .unwrap();

    let searched = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{active_id}/missing-resource-searches"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(searched.status(), StatusCode::OK);
    let body = json_body(searched).await;
    assert_eq!(body["data"]["queued"], true);
    assert_eq!(body["data"]["reset_count"], 1);
    let history = store.load_wanted_history(subscribe_id).unwrap();
    let reset = history.get(&(None, None)).unwrap();
    assert_eq!(reset.search_attempts, 0);
    assert_eq!(reset.last_search_at, None);

    let paused = create_api_subscribe(&app, "Paused Movie", "management-secret").await;
    let paused_id = paused["id"].as_str().unwrap();
    let pause_response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{paused_id}"),
            Some("management-secret"),
            json!({ "tracking_state": "paused" }),
        ))
        .await
        .unwrap();
    assert_eq!(pause_response.status(), StatusCode::OK);
    let paused_search = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{paused_id}/missing-resource-searches"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(paused_search.status(), StatusCode::CONFLICT);

    let complete = create_api_subscribe(&app, "Complete Movie", "management-secret").await;
    let complete_id = SubscribeId::from_str(complete["id"].as_str().unwrap()).unwrap();
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 100,
            path: Some("/library/Complete.Movie.mkv".into()),
        },
    );
    store.save_subscribe_facts(complete_id, &facts).unwrap();
    let complete_search = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{complete_id}/missing-resource-searches"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(complete_search.status(), StatusCode::CONFLICT);
}

struct GatedCatalog {
    gate: Arc<tokio::sync::Notify>,
    entered: std::sync::atomic::AtomicBool,
    released: parking_lot::Mutex<bool>,
    cond: parking_lot::Condvar,
}
impl GatedCatalog {
    fn new() -> Self {
        Self {
            gate: Arc::new(tokio::sync::Notify::new()),
            entered: std::sync::atomic::AtomicBool::new(false),
            released: parking_lot::Mutex::new(false),
            cond: parking_lot::Condvar::new(),
        }
    }
    fn release(&self) {
        *self.released.lock() = true;
        self.cond.notify_all();
    }
}
impl api::catalog::Catalog for GatedCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn season_episodes(&self, _: &str, _: u32) -> Result<Vec<media::SeasonEpisode>, String> {
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        self.gate.notify_waiters();
        let mut released = self.released.lock();
        self.cond
            .wait_while_for(&mut released, |r| !*r, std::time::Duration::from_secs(5));
        assert!(*released, "test must release season_episodes gate");
        Ok(vec![media::SeasonEpisode {
            episode_number: 1,
            air_date: Some("2020-01-01".into()),
        }])
    }
}

#[tokio::test]
async fn missing_search_releases_subscribe_guard_during_aired_cap_and_rejects_deleting() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(GatedCatalog::new());
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(catalog.clone());
    let app = router(api_state.clone());
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "Open Show", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                    "follow_future": false,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    let sid: SubscribeId = id.parse().unwrap();

    // 1. Mark deleting -> 409 subscription.deleting
    api_state.mark_subscribe_deleting(sid);
    let deleting = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/missing-resource-searches"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleting.status(), StatusCode::CONFLICT);
    assert_eq!(
        super::common::json_error(deleting).await["code"],
        "subscription.deleting"
    );
    api_state.clear_subscribe_deleting(sid);

    // 2. While aired_cap blocks in Catalog::season_episodes, PATCH completes promptly.
    let search_app = app.clone();
    let search_uri = format!("/api/v1/subscriptions/{id}/missing-resource-searches");
    let search = tokio::spawn(async move {
        search_app
            .oneshot(request(
                "POST",
                &search_uri,
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !catalog.entered.load(std::sync::atomic::Ordering::Acquire) {
            catalog.gate.notified().await;
        }
    })
    .await
    .expect("missing search must reach Catalog::season_episodes");
    let patched = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        app.oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "follow_future": true }),
        )),
    )
    .await
    .expect("PATCH must not block on subscribe_guard during aired_cap HTTP")
    .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    catalog.release();
    assert_eq!(search.await.unwrap().status(), StatusCode::OK);
}
