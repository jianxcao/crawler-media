#[path = "management/common.rs"]
mod common;
use axum::http::StatusCode;
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;

#[tokio::test]
async fn frozen_library_and_root_return_503_without_leaking_to_unauthorized_users() {
    let tmp = tempfile::tempdir().unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(vec![]),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = api::router(state.clone());
    let (_, token) = common::create_member(&app, "reader").await;
    let (library, other) = {
        let shared = state.store();
        let store = shared.as_ref();
        let library = store
            .create_library(
                domain::MediaKind::Movie,
                "Frozen",
                &["/frozen"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let other = store
            .create_library(
                domain::MediaKind::Movie,
                "Other",
                &["/other"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .begin_library_integrity(library.id.parse().unwrap(), true, "freeze", 0)
            .unwrap();
        (library.id, other.id)
    };
    for uri in [
        format!("/api/v1/libraries/{library}/items"),
        format!("/api/v1/libraries/{library}/facets"),
        format!("/Items?ParentId={library}"),
        "/Items".into(),
    ] {
        let response = app
            .clone()
            .oneshot(common::request(
                "GET",
                &uri,
                Some(&token),
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
    }
    let response = app
        .clone()
        .oneshot(common::request(
            "GET",
            &format!("/Items?ParentId={other}"),
            Some(&token),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    {
        let shared = state.store();
        let store = shared.as_ref();
        store.advance_library_operation("freeze", 1, 100).unwrap();
    }
    let response = app
        .clone()
        .oneshot(common::request(
            "GET",
            "/Items",
            Some(&token),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_hidden_library(app, &state, &token).await;
}

async fn assert_hidden_library(app: axum::Router, state: &api::ApiState, token: &str) {
    let hidden = {
        let shared = state.store();
        let store = shared.as_ref();
        let hidden = store
            .create_library(
                domain::MediaKind::Movie,
                "Hidden",
                &["/hidden"],
                "members",
                true,
                &[],
            )
            .unwrap();
        store
            .begin_library_integrity(hidden.id.parse().unwrap(), false, "hidden", 2)
            .unwrap();
        hidden.id
    };
    let response = app
        .clone()
        .oneshot(common::request(
            "GET",
            &format!("/api/v1/libraries/{hidden}/items"),
            Some(token),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .oneshot(common::request(
            "GET",
            "/Items",
            Some(token),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn http_integrity_start_drives_check_and_repair_without_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(vec![]),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = api::router(state.clone());
    let library = {
        let shared = state.store();
        let store = shared.as_ref();
        store
            .create_library(
                domain::MediaKind::Movie,
                "Driven",
                &["/driven"],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    for action in ["integrity-checks", "integrity-repairs"] {
        let response = app
            .clone()
            .oneshot(common::request(
                "POST",
                &format!("/api/v1/libraries/{library}/{action}"),
                Some("management-secret"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = common::json_body(response).await;
        let operation = body["data"]["operation_id"].as_str().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let finished = {
                    let shared = state.store();
                    let store = shared.as_ref();
                    store
                        .library_operation_view(operation)
                        .unwrap()
                        .unwrap()
                        .state
                        == "succeeded"
                };
                if finished {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
