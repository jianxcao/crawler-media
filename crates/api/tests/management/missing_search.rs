use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::SubscribeId;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use subscribe::{QualityFact, SubscribeFacts};
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
