use super::*;

async fn listed_library(f: &PresenceFixture) -> Value {
    let (status, body) = f
        .request("GET", "/api/v1/libraries?scope=admin", Some(&f.admin_token))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["id"] == f.library_id)
        .unwrap()
        .clone()
}

async fn patch(f: &PresenceFixture, body: Value) -> (StatusCode, Value) {
    f.request_with_body(
        "PATCH",
        &format!("/api/v1/libraries/{}", f.library_id),
        Some(&f.admin_token),
        body,
    )
    .await
}

#[tokio::test]
async fn rejected_roots_roll_back_every_library_setting() {
    let f = PresenceFixture::new();
    f.state
        .store()
        .set_library_intro_settings(&f.library_id, true, true)
        .unwrap();
    let original = listed_library(&f).await;
    f.insert_media_and_ledger("Recorded.mkv");
    let work = f.state.acquire_library_work(&f.library_id).unwrap();
    for roots in [
        json!(["relative-test"]),
        json!([f.dir.path().join("other")]),
    ] {
        let (status, response) = patch(
            &f,
            json!({
                "name": "Rejected", "admin_visible": false,
                "detect_intros": false, "enable_fingerprint": false,
                "exclude_from_home": true, "root_paths": roots,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        let current = listed_library(&f).await;
        for field in [
            "name",
            "admin_visible",
            "root_paths",
            "detect_intros",
            "enable_fingerprint",
            "exclude_from_home",
        ] {
            assert_eq!(current[field], original[field], "{field}");
        }
        assert!(
            f.state
                .store()
                .active_library_operation(&f.library_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.state
                .store()
                .library_lifecycle_state(&f.library_id)
                .unwrap(),
            "ready"
        );
        assert!(!work.control().is_cancelled());
    }
}

#[tokio::test]
async fn conflicting_patch_does_not_commit_settings() {
    let f = PresenceFixture::new();
    f.state
        .store()
        .set_library_intro_settings(&f.library_id, true, true)
        .unwrap();
    let initial = f.state.store().get_library(&f.library_id).unwrap().unwrap();
    f.state
        .store()
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: "winner".into(),
                library_id: f.library_id.parse().unwrap(),
                expected_revision: 0,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            1,
        )
        .unwrap();
    let (status, response) = patch(
        &f,
        json!({
            "name": "Loser", "detect_intros": false, "enable_fingerprint": false,
            "display_locale": "ja",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(response["error"]["operation_id"], "winner");
    let current = f.state.store().get_library(&f.library_id).unwrap().unwrap();
    assert_eq!(current.name, initial.name);
    assert!(current.detect_intros && current.enable_fingerprint);
    assert_eq!(
        f.state
            .store()
            .active_library_operation(&f.library_id)
            .unwrap()
            .as_deref(),
        Some("winner")
    );
}

async fn wait_operation(f: &PresenceFixture, operation: &Value) {
    let id = operation["data"]["operation_id"].as_str().unwrap();
    for _ in 0..200 {
        let (status, body) = f
            .request(
                "GET",
                &format!("/api/v1/libraries/{}/operations/{id}", f.library_id),
                Some(&f.admin_token),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        match body["data"]["state"].as_str().unwrap() {
            "succeeded" => return,
            "failed" => panic!("{body}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    panic!("Library operation did not complete");
}

#[tokio::test]
async fn successful_patch_commits_settings_and_a_pollable_operation() {
    let f = PresenceFixture::new();
    let new_root = f.dir.path().join("replacement");
    let (status, response) = patch(
        &f,
        json!({
            "name": "Accepted", "admin_visible": false, "root_paths": [new_root],
            "detect_intros": true, "enable_fingerprint": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    wait_operation(&f, &response).await;
    let library = listed_library(&f).await;
    assert_eq!(library["name"], "Accepted");
    assert_eq!(library["admin_visible"], false);
    assert_eq!(library["root_paths"], json!([new_root]));
    assert_eq!(library["detect_intros"], true);
    let (status, response) = patch(&f, json!({"name": "Synchronous"})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["data"]["name"], "Synchronous");
    assert!(response["data"].get("operation_id").is_none());
}

#[tokio::test]
async fn empty_locale_clears_override_through_a_durable_operation() {
    let f = PresenceFixture::new();
    let (status, explicit) = patch(&f, json!({"display_locale": "zh-CN"})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{explicit}");
    wait_operation(&f, &explicit).await;
    let (status, cleared) = patch(&f, json!({"display_locale": ""})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{cleared}");
    assert_eq!(cleared["data"]["kind"], "locale");
    wait_operation(&f, &cleared).await;
    let (status, response) = patch(&f, json!({"name": "Inherited"})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        store::library_query::details::library_display_locale(
            &f.state.store(),
            f.library_id.parse().unwrap()
        )
        .unwrap()
        .as_deref(),
        Some("zh-CN")
    );
}

#[tokio::test]
async fn operation_registration_failure_rolls_back_settings_and_preserves_work() {
    let f = PresenceFixture::new();
    f.state
        .store()
        .set_library_intro_settings(&f.library_id, true, true)
        .unwrap();
    let original = listed_library(&f).await;
    let work = f.state.acquire_library_work(&f.library_id).unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_operation BEFORE INSERT ON library_operations BEGIN SELECT RAISE(ABORT,'operation registration failed'); END;").unwrap();
    let (status, response) = patch(
        &f,
        json!({
            "name": "Rejected", "detect_intros": false, "enable_fingerprint": false,
            "admin_visible": false, "display_locale": "en-US",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{response}");
    assert_eq!(listed_library(&f).await, original);
    assert!(!work.control().is_cancelled());
    assert!(f.state.acquire_library_work(&f.library_id).is_ok());
    assert_eq!(
        f.state
            .store()
            .library_config_revision(&f.library_id)
            .unwrap(),
        0
    );
    assert!(
        f.state
            .store()
            .unfinished_library_operations()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn operation_winning_after_middleware_admission_refuses_all_patch_fields() {
    use axum::{
        body::{Body, Bytes},
        http::Request,
    };
    use tokio_stream::StreamExt;
    use tower::ServiceExt;
    for reconfigure in [false, true] {
        let f = PresenceFixture::new();
        f.state
            .store()
            .set_library_intro_settings(&f.library_id, true, true)
            .unwrap();
        let mut body =
            json!({"name": "Loser", "detect_intros": false, "enable_fingerprint": false});
        if reconfigure {
            body["display_locale"] = json!("ja");
        }
        let store = f.state.store();
        let id = f.library_id.clone();
        let stream = tokio_stream::iter([Bytes::from(body.to_string())]).map(move |bytes| {
            let store = store.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                store
                    .begin_library_reconfiguration(
                        &store::LibraryReconfiguration {
                            operation_id: "winner-after-admission".into(),
                            library_id: id.parse().unwrap(),
                            expected_revision: 0,
                            root_paths: None,
                            display_locale: Some("en-US".into()),
                        },
                        1,
                    )
                    .unwrap()
            })
            .join()
            .unwrap();
            Ok::<_, std::io::Error>(bytes)
        });
        let response = api::management::router(f.state.clone())
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/libraries/{}", f.library_id))
                    .header("Authorization", format!("Bearer {}", f.admin_token))
                    .header("Content-Type", "application/json")
                    .body(Body::from_stream(stream))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["error"]["code"], "library.reindexing");
        assert_eq!(response["error"]["operation_id"], "winner-after-admission");
        let current = f.state.store().get_library(&f.library_id).unwrap().unwrap();
        assert_eq!(current.name, "Movies");
        assert!(current.detect_intros && current.enable_fingerprint);
    }
}
