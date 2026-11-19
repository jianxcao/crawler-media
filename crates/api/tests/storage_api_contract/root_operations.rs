use super::*;

#[tokio::test]
async fn root_conflict_rejects_creation_and_all_patch_fields_without_cancelling_work() {
    let f = PresenceFixture::new();
    let root = f.dir.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    f.state
        .store()
        .create_library(
            domain::MediaKind::Movie,
            "Other",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    f.state
        .store()
        .set_library_intro_settings(&f.library_id, true, true)
        .unwrap();
    let before = listed_library(&f).await;
    let work = f.state.acquire_library_work(&f.library_id).unwrap();
    let (status, body) = patch(
        &f,
        json!({
            "name":"Rejected", "access_mode":"selected", "admin_visible":false,
            "detect_intros":false, "enable_fingerprint":false, "exclude_from_home":true,
            "root_paths":[f.root, root],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "library.root_conflict");
    assert_eq!(listed_library(&f).await, before);
    assert!(!work.control().is_cancelled());
    assert!(
        f.state
            .store()
            .active_library_operation(&f.library_id)
            .unwrap()
            .is_none()
    );
    let (status, body) = f
        .request_with_body(
            "POST",
            "/api/v1/libraries",
            Some(&f.admin_token),
            json!({
                "kind":"movie", "name":"Rejected creation", "root_paths":[root],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "library.root_conflict");
    assert!(
        !f.state
            .store()
            .list_libraries()
            .unwrap()
            .iter()
            .any(|library| library.name == "Rejected creation")
    );
}

fn failed_roots(f: &PresenceFixture) -> String {
    let operation = "failed-roots";
    f.state
        .store()
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: operation.into(),
                library_id: f.library_id.parse().unwrap(),
                expected_revision: 0,
                root_paths: Some(vec![f.dir.path().join("old-plan")]),
                display_locale: Some("en-US".into()),
            },
            1,
        )
        .unwrap();
    f.state
        .store()
        .fail_library_operation_if_owned(operation, "interrupted first batch", 2)
        .unwrap();
    operation.into()
}

fn stored_plan(f: &PresenceFixture, operation: &str) -> (String, Option<String>) {
    rusqlite::Connection::open(f.dir.path().join("data/library.db"))
        .unwrap()
        .query_row(
            "SELECT plan_json,cursor_json FROM library_operations WHERE operation_id=?1",
            [operation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[tokio::test]
async fn failed_root_plan_can_be_corrected_without_changing_operation_or_frozen_revision() {
    let f = PresenceFixture::new();
    let operation = failed_roots(&f);
    let replacement = f.dir.path().join("corrected");
    let (status, body) = f
        .request_with_body(
            "POST",
            &format!(
                "/api/v1/libraries/{}/operations/{operation}/retry",
                f.library_id
            ),
            Some(&f.admin_token),
            json!({
                "expected_revision":1, "root_paths":[replacement],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["data"]["operation_id"], operation);
    wait_retried_operation(&f, &body).await;
    assert_eq!(
        f.state
            .store()
            .get_library(&f.library_id)
            .unwrap()
            .unwrap()
            .root_paths,
        vec![replacement]
    );
    assert_eq!(
        f.state
            .store()
            .library_config_revision(&f.library_id)
            .unwrap(),
        1
    );
    assert_eq!(
        store::library_query::details::library_display_locale(
            &f.state.store(),
            f.library_id.parse().unwrap()
        )
        .unwrap()
        .as_deref(),
        Some("en-US")
    );
}

#[tokio::test]
async fn rejected_root_plan_corrections_preserve_the_failed_plan_and_cursor() {
    let f = PresenceFixture::new();
    let operation = failed_roots(&f);
    let target = f.dir.path().join("taken");
    f.state
        .store()
        .create_library(
            domain::MediaKind::Movie,
            "Other",
            &[target.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let before = stored_plan(&f, &operation);
    let retry = format!(
        "/api/v1/libraries/{}/operations/{operation}/retry",
        f.library_id
    );
    for (body, code) in [
        (
            json!({"expected_revision":0,"root_paths":[f.root]}),
            "library.operation_not_owner",
        ),
        (
            json!({"expected_revision":1,"root_paths":[target]}),
            "library.root_conflict",
        ),
    ] {
        let (status, response) = f
            .request_with_body("POST", &retry, Some(&f.admin_token), body)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{response}");
        assert_eq!(response["error"]["code"], code);
        assert_eq!(stored_plan(&f, &operation), before);
        assert_eq!(
            f.state
                .store()
                .library_operation_view(&operation)
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
    }
    // A cursor is proof that at least one batch committed, so only an ordinary retry is legal.
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    db.execute(
        "UPDATE library_operations SET cursor_json='{}' WHERE operation_id=?1",
        [&operation],
    )
    .unwrap();
    let before = stored_plan(&f, &operation);
    let (status, response) = f
        .request_with_body(
            "POST",
            &retry,
            Some(&f.admin_token),
            json!({"expected_revision":1,"root_paths":[f.root]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        response["error"]["code"],
        "library.operation_not_replannable"
    );
    assert_eq!(stored_plan(&f, &operation), before);
}

#[tokio::test]
async fn root_plan_correction_validates_body_and_claim_before_modifying_plan() {
    let f = PresenceFixture::new();
    let work = f.state.acquire_library_work(&f.library_id).unwrap();
    let operation = failed_roots(&f);
    let before = stored_plan(&f, &operation);
    let retry = format!(
        "/api/v1/libraries/{}/operations/{operation}/retry",
        f.library_id
    );
    for body in [
        json!({}),
        json!({"expected_revision":1}),
        json!({"expected_revision":1,"root_paths":[]}),
        json!({"expected_revision":1,"root_paths":["relative"]}),
        json!({"expected_revision":1,"root_paths":[f.root],"extra":true}),
    ] {
        let (status, response) = f
            .request_with_body("POST", &retry, Some(&f.admin_token), body)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_eq!(stored_plan(&f, &operation), before);
    }
    let (status, accepted) = f.request("POST", &retry, Some(&f.admin_token)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let (status, response) = f
        .request_with_body(
            "POST",
            &retry,
            Some(&f.admin_token),
            json!({"expected_revision":1,"root_paths":[f.root]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(response["error"]["code"], "library.operation_running");
    assert_eq!(stored_plan(&f, &operation), before);
    drop(work);
    wait_retried_operation(&f, &accepted).await;
}

async fn wait_retried_operation(f: &PresenceFixture, accepted: &Value) {
    let operation = accepted["data"]["operation_id"].as_str().unwrap();
    let path = format!("/api/v1/libraries/{}/operations/{operation}", f.library_id);
    for _ in 0..200 {
        let (status, body) = f.request("GET", &path, Some(&f.admin_token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["data"]["state"] == "succeeded" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("retried operation did not succeed");
}
