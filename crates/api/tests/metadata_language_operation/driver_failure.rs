use super::*;
use std::time::Duration;

#[tokio::test]
async fn undrained_work_fails_durably_and_explicit_retry_recovers() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = stage(&temp);
    let library = state
        .store()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    let work = state.acquire_library_work(&library.id).unwrap();
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/settings/metadata-language",
            serde_json::json!({"expected_revision":0,"language":"ja"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let changed = body_json(response).await;
    let operation = changed["data"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["library_id"] == library.id)
        .unwrap();
    let id = operation["operation_id"].as_str().unwrap();
    let poll = format!("/api/v1/libraries/{}/operations/{id}", library.id);
    assert!(work.control().is_cancelled());
    let failed = tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let response = app.clone().oneshot(request("GET", &poll)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let view = body_json(response).await;
            if view["data"]["state"] == "failed" {
                break view;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("undrained work must reach durable failure at the drain deadline");
    assert!(failed["data"]["error"].as_str().unwrap().contains("drain:"));
    assert!(failed["data"]["cursor"].is_null());
    assert_eq!(
        state.store().library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );
    drop(work);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = app
                .clone()
                .oneshot(request("POST", &format!("{poll}/retry")))
                .await
                .unwrap();
            if response.status() == StatusCode::ACCEPTED {
                assert_eq!(body_json(response).await["data"]["operation_id"], id);
                break;
            }
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                body_json(response).await["error"]["code"],
                "library.operation_running"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry must be admitted after old failure cleanup releases its claim");
    wait_for_operation(&app, &poll).await;
    assert_eq!(
        state.store().library_lifecycle_state(&library.id).unwrap(),
        "ready"
    );
}
