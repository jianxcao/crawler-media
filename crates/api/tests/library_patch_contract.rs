use axum::http::StatusCode;
use domain::MediaKind;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;
#[path = "management/common.rs"]
mod common;

#[tokio::test]
async fn retired_and_invalid_patch_fields_do_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let library = state
        .store()
        .lock()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap();
    let app = api::router(state.clone());
    for body in [
        json!({"name":"must-not-write", "source":"local"}),
        json!({"auto_clear_missing":true}),
        json!({"scrape_overrides":{"language":"en-US"}}),
        json!({"name":"must-not-write", "detect_intros":"yes"}),
        json!({"kind":"tv"}),
    ] {
        let response = app
            .clone()
            .oneshot(common::request(
                "PATCH",
                &format!("/api/v1/libraries/{}", library.id),
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            state
                .store()
                .lock()
                .get_library(&library.id)
                .unwrap()
                .unwrap()
                .name,
            library.name
        );
    }
    let response = app
        .oneshot(common::request(
            "PATCH",
            &format!("/api/v1/libraries/{}", library.id),
            Some("management-secret"),
            json!({"name":"updated", "default_filter_id":null}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value = common::json_data(response).await;
    assert_eq!(result["name"], "updated");
}
