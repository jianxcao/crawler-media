//! Ownership, recurrent content and distinct versions through the public router.
use super::*;
use serde_json::json;

#[test]
fn current_status_detects_actual_a_b_a_recurrence() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let file = &f.files[0];
    let a = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    f.store
        .put_cached_chapters(&file.file_id.to_string(), &[chapter(100, 200)])
        .unwrap();
    f.store
        .put_cached_chapters(
            &file.file_id.to_string(),
            &[chapter(0, 10_000), chapter(20_000, 30_000)],
        )
        .unwrap();
    let recurrent = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(a.status.generation, recurrent.status.generation);
    assert_ne!(a.task.id, recurrent.task.id);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let observed = f
            .frame_status(
                0,
                &format!("&task_id={}&generation={}", a.task.id, a.status.generation),
            )
            .await;
        assert_eq!(observed["superseded"], true);
        assert_eq!(observed["task"]["task_id"], recurrent.task.id);
    });
}

#[test]
fn selected_versions_keep_distinct_chapters_tasks_and_generated_images() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    f.store
        .put_cached_chapters(&f.files[1].file_id.to_string(), &[chapter(40_000, 50_000)])
        .unwrap();
    let first = f.publish(0);
    let second = f.publish(1);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for (which, start, image) in [(0, 0, &first[0].0), (1, 40_000, &second[0].0)] {
            let response = f
                .chapter_request(which, "GET", "", "", Value::Null, Some("management-secret"))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = common::json_body(response).await;
            let selected = body["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["start_ms"] == start)
                .unwrap();
            assert_eq!(selected["image_url"], *image);
            let response = f
                .chapter_request(
                    which,
                    "POST",
                    "/generate",
                    "",
                    json!({"retry_failed": false}),
                    Some("management-secret"),
                )
                .await;
            let generated = common::json_body(response).await;
            assert_eq!(generated["data"]["task"]["state"], "completed");
            assert_eq!(
                generated["data"]["task"],
                f.frame_status(which, "").await["task"]
            );
        }
    });
}

#[test]
fn member_status_is_visible_only_in_authorized_library_and_generate_stays_admin() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let user = domain::User {
        id: domain::UserId::new(),
        login: "viewer".into(),
        enabled: true,
        role: domain::UserRole::Member,
    };
    f.store.insert_user(&user).unwrap();
    f.store.set_user_token(user.id, "member-fixture").unwrap();
    let file = &f.files[0];
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_eq!(
            f.chapter_request(
                0,
                "GET",
                "/frame-status",
                "",
                Value::Null,
                Some("member-fixture")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            f.chapter_request(
                0,
                "POST",
                "/generate",
                "",
                json!({"retry_failed": false}),
                Some("member-fixture")
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        f.store
            .update_library(
                &file.library_id.to_string(),
                None,
                None,
                Some(("selected", true, &[])),
            )
            .unwrap();
        let current = f
            .store
            .current_chapter_frame_task(file.file_id)
            .unwrap()
            .unwrap();
        let response = f
            .chapter_request(
                0,
                "GET",
                "/frame-status",
                &format!(
                    "&task_id={}&generation={}",
                    current.task.id, current.status.generation
                ),
                Value::Null,
                Some("member-fixture"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !common::json_body(response)
                .await
                .to_string()
                .contains(&current.task.id)
        );
    });
}

#[test]
fn disabled_chapter_images_are_rejected_instead_of_reported_as_empty() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    f.store
        .set_library_switch_settings(
            &f.files[0].library_id.to_string(),
            None,
            None,
            Some(false),
            None,
            None,
        )
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let response = f
            .chapter_request(
                0,
                "POST",
                "/generate",
                "",
                json!({"retry_failed":false}),
                Some("management-secret"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            common::json_body(response).await["error"]["code"],
            "chapters.disabled"
        );
    });
}
