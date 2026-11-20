//! A completed Library removal permanently closes its Subscribe target identity.
use super::support::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{MediaKind, SubscribeId};
use serde_json::json;
use tower::ServiceExt;

#[test]
fn closed_library_rejects_stale_subscribe_update_and_keeps_default_transfer() {
    let fixture = Fixture::new(MediaKind::Movie);
    fixture.pending(MediaKind::Movie);
    let store = fixture.state.store();
    let mut stale = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    runtime().block_on(async {
        let operation = delete(&fixture).await;
        stale.tracking_state = "paused".into();
        assert!(matches!(
            store.update_subscribe(&stale),
            Err(store::StoreError::Conflict(_))
        ));
        assert_eq!(fixture.binding(), None);
        assert_eq!(
            store
                .library_removal_status(&operation)
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id))
            .await
            .unwrap()
            .unwrap();
        let rows = store.file_coverage_for_media(fixture.media).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file.library_id.to_string(), fixture.default.id);
    });
}

#[test]
fn closed_library_rejects_a_creator_retained_before_detachment() {
    let fixture = Fixture::new(MediaKind::Movie);
    let store = fixture.state.store();
    let mut late = store.get_subscribe(fixture.subscribe).unwrap().unwrap();
    late.id = SubscribeId::new();
    let creation = store.begin_subscribe_creation(&late).unwrap();
    store.retain_subscribe_creation(&creation).unwrap();
    runtime().block_on(async {
        let operation = delete(&fixture).await;
        assert!(matches!(
            store.commit_subscribe_creation(&creation, &late, false),
            Err(store::StoreError::Conflict(_))
        ));
        assert!(store.get_subscribe(late.id).unwrap().is_none());
        store.abort_subscribe_creation(&creation).unwrap();
        assert_eq!(
            store
                .library_removal_status(&operation)
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
    });
}

#[test]
fn closed_library_target_stays_closed_after_reopening_store() {
    let fixture = Fixture::new(MediaKind::Movie);
    let mut late = fixture
        .state
        .store()
        .get_subscribe(fixture.subscribe)
        .unwrap()
        .unwrap();
    late.id = SubscribeId::new();
    runtime().block_on(async { delete(&fixture).await });
    let reopened = store::Store::open(fixture.temp.path().join("data")).unwrap();
    assert!(matches!(
        reopened.insert_subscribe(&late),
        Err(store::StoreError::Conflict(_))
    ));
    assert!(reopened.get_subscribe(late.id).unwrap().is_none());
    assert_eq!(fixture.binding(), None);
}

#[test]
fn closed_or_missing_library_cannot_be_explicitly_selected_over_http() {
    let fixture = Fixture::new(MediaKind::Movie);
    runtime().block_on(async {
        let operation = delete(&fixture).await;
        for id in [
            &fixture.removed.id,
            &domain::LibraryId::new().to_string(),
            "not-a-uuid",
        ] {
            let response = fixture
                .app()
                .oneshot(json_request(
                    "POST",
                    "/api/v1/subscriptions",
                    json!({
                        "media": {"kind": "movie", "title": "Rejected closed target"},
                        "coverage": {"kind": "movie"}, "library_id": id,
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "target {id}");
        }
        let response = fixture
            .app()
            .oneshot(json_request(
                "PATCH",
                &format!("/api/v1/subscriptions/{}", fixture.subscribe),
                json!({"library_id": fixture.removed.id}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(fixture.binding(), None);
        assert_eq!(
            fixture
                .state
                .store()
                .library_removal_status(&operation)
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
    });
}

async fn delete(fixture: &Fixture) -> String {
    let response = fixture
        .app()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{}", fixture.removed.id),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = super::support::json(response).await;
    let id = body["data"]["operation_id"].as_str().unwrap().to_string();
    removed(fixture, &id).await;
    id
}

fn json_request(method: &str, path: &str, value: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer removal-subscribe-secret")
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
