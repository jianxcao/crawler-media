//! Overlap admission and mutation must share one Subscribe writer decision.
mod subscription_coverage_atomicity {
    pub mod support;
}
use axum::http::StatusCode;
use serde_json::{Value, json};
use subscription_coverage_atomicity::support::*;

#[test]
fn concurrent_create_keeps_one_subscription_and_its_search_job() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let uri = "/api/v1/subscriptions".to_string();
        let responses = fixture
            .concurrent(
                "POST",
                [uri.clone(), uri.clone()],
                [fixture.body(1, 5), fixture.body(1, 5)],
                false,
            )
            .await;
        let mut statuses = responses.iter().map(|r| r.0.as_u16()).collect::<Vec<_>>();
        statuses.sort();
        assert_eq!(statuses, [201, 409], "{responses:?}");
        let conflict = responses
            .iter()
            .find(|r| r.0 == StatusCode::CONFLICT)
            .unwrap();
        assert_eq!(conflict.1["error"]["code"], "subscription.overlap");
        let (status, listing) = fixture.call("GET", &uri, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{listing}");
        let subscriptions = listing["data"].as_array().unwrap();
        assert_eq!(subscriptions.len(), 1, "{listing}");
        let winner = subscriptions[0]["id"].as_str().unwrap();
        assert_eq!(fixture.search_job_subscribes(), [winner]);
        let persisted = fixture.state.store().list_all_subscribes().unwrap();
        assert_eq!(
            persisted
                .iter()
                .filter(|sub| sub.media_id == fixture.media)
                .count(),
            1
        );
    });
}

#[test]
fn concurrent_coverage_patches_keep_the_two_ranges_disjoint() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let mut ids = Vec::new();
        for (from, to) in [(1, 3), (7, 9)] {
            let (status, body) = fixture
                .call("POST", "/api/v1/subscriptions", fixture.body(from, to))
                .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            ids.push(body["data"]["id"].as_str().unwrap().to_string());
        }
        let patch = json!({ "coverage": coverage(4, 6) });
        let responses = fixture
            .concurrent(
                "PATCH",
                [
                    format!("/api/v1/subscriptions/{}", ids[0]),
                    format!("/api/v1/subscriptions/{}", ids[1]),
                ],
                [patch.clone(), patch],
                true,
            )
            .await;
        let mut statuses = responses.iter().map(|r| r.0.as_u16()).collect::<Vec<_>>();
        statuses.sort();
        assert_eq!(statuses, [200, 409], "{responses:?}");
        let conflict = responses
            .iter()
            .find(|r| r.0 == StatusCode::CONFLICT)
            .unwrap();
        assert_eq!(conflict.1["error"]["code"], "subscription.overlap");
        let (_, body) = fixture
            .call("GET", "/api/v1/subscriptions", Value::Null)
            .await;
        let subscriptions = body["data"].as_array().unwrap();
        assert_eq!(subscriptions.len(), 2, "{body}");
        assert_eq!(
            subscriptions
                .iter()
                .filter(|sub| sub["coverage"] == coverage(4, 6))
                .count(),
            1,
            "{body}"
        );
        let mut scheduled = fixture.search_job_subscribes();
        scheduled.sort();
        ids.sort();
        assert_eq!(scheduled, ids);
    });
}

#[test]
fn scheduling_patch_preserves_existing_detached_subscription_coverage() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let mut ids = Vec::new();
        for (from, to) in [(1, 3), (7, 9)] {
            let (status, body) = fixture
                .call("POST", "/api/v1/subscriptions", fixture.body(from, to))
                .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            ids.push(body["data"]["id"].as_str().unwrap().to_string());
        }
        // Library removal can detach subscriptions that had equal coverage in
        // different libraries. Persist that allowed detached state directly.
        let store = fixture.state.store();
        for id in &ids {
            let mut subscribe = store.get_subscribe(id.parse().unwrap()).unwrap().unwrap();
            subscribe.library_id = None;
            subscribe.coverage = domain::Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(3),
            };
            store.update_subscribe(&subscribe).unwrap();
        }
        let (status, body) = fixture
            .call(
                "PATCH",
                &format!("/api/v1/subscriptions/{}", ids[0]),
                json!({ "tracking_state": "paused", "search_interval_secs": 7200 }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let persisted = store
            .get_subscribe(ids[0].parse().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(persisted.tracking_state, "paused");
        assert_eq!(persisted.search_interval_secs, 7200);
        assert_eq!(persisted.library_id, None);
        assert_eq!(
            persisted.coverage,
            domain::Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(3),
            }
        );
        assert_eq!(store.list_all_subscribes().unwrap().len(), 2);
    });
}
