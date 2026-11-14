//! Selected-file HTTP status is read-only and reports actual immutable images.
use super::*;
use serde_json::json;

impl Fixture {
    pub(super) async fn chapter_request(
        &self,
        which: usize,
        method: &str,
        action: &str,
        extra: &str,
        body: Value,
        token: Option<&str>,
    ) -> axum::response::Response {
        let file = &self.files[which];
        let url = format!(
            "/api/v1/libraries/{}/items/{}/chapters{action}?file_id={}&season=1&episode=1{extra}",
            file.library_id, file.media_id, file.file_id
        );
        self.app
            .clone()
            .oneshot(common::request(method, &url, token, body))
            .await
            .unwrap()
    }
    pub(super) async fn frame_status(&self, which: usize, extra: &str) -> Value {
        let response = self
            .chapter_request(
                which,
                "GET",
                "/frame-status",
                extra,
                Value::Null,
                Some("management-secret"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        common::json_body(response).await["data"].clone()
    }
}

#[test]
fn chapter_status_selects_file_and_detects_recurrent_task_identity() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let a = f
        .store
        .current_chapter_frame_task(f.files[0].file_id)
        .unwrap()
        .unwrap();
    let b = f
        .store
        .current_chapter_frame_task(f.files[1].file_id)
        .unwrap()
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_eq!(f.frame_status(0, "").await["task"]["task_id"], a.task.id);
        assert_eq!(f.frame_status(1, "").await["task"]["task_id"], b.task.id);
        let old = format!("&task_id=historical-a&generation={}", a.status.generation);
        let current = f.frame_status(0, &old).await;
        assert_eq!(current["superseded"], true);
        assert_eq!(current["task"]["task_id"], a.task.id);
        for extra in ["&task_id=alone", "&generation=alone"] {
            assert_eq!(
                f.chapter_request(
                    0,
                    "GET",
                    "/frame-status",
                    extra,
                    Value::Null,
                    Some("management-secret")
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            f.chapter_request(0, "GET", "/frame-status", "", Value::Null, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let result = f
            .chapter_request(
                0,
                "POST",
                "/generate",
                "",
                json!({"retry_failed": false}),
                Some("management-secret"),
            )
            .await;
        assert_eq!(result.status(), StatusCode::OK);
        let result = common::json_body(result).await;
        assert_eq!(result["data"]["task"]["task_id"], a.task.id);
        assert_eq!(result["data"]["task"]["state"], "queued");
        assert_eq!(result["data"]["empty"], false);
        let again = f
            .chapter_request(
                0,
                "POST",
                "/generate",
                "",
                json!({"retry_failed": false}),
                Some("management-secret"),
            )
            .await;
        assert_eq!(common::json_body(again).await["data"], result["data"]);
    });
}

#[test]
fn completed_status_projects_missing_or_corrupt_artifacts_without_repairing() {
    for corrupt in [false, true] {
        let f = Fixture::new(&[UnitKey::new(1, 1)]);
        let frames = f.publish(0);
        let before = f
            .store
            .current_chapter_frame_task(f.files[0].file_id)
            .unwrap()
            .unwrap();
        let modified = std::fs::metadata(&frames[1].2).unwrap().modified().unwrap();
        if corrupt {
            std::fs::write(&frames[0].2, b"corrupt").unwrap();
        } else {
            std::fs::remove_file(&frames[0].2).unwrap();
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let status = f.frame_status(0, "").await;
            assert_eq!(status["task"]["state"], "partial");
            assert_eq!(status["task"]["ready"], 1);
            assert_eq!(status["task"]["failed"], 1);
            assert!(status["task"]["targets"][0]["error"].is_string());
            assert_eq!(
                f.store
                    .current_chapter_frame_task(f.files[0].file_id)
                    .unwrap()
                    .unwrap(),
                before
            );
            let repaired = f
                .chapter_request(
                    0,
                    "POST",
                    "/generate",
                    "",
                    json!({"retry_failed": true}),
                    Some("management-secret"),
                )
                .await;
            assert_eq!(repaired.status(), StatusCode::OK);
            assert_eq!(
                common::json_body(repaired).await["data"]["task"]["state"],
                "queued"
            );
        });
        let after = f
            .store
            .current_chapter_frame_task(f.files[0].file_id)
            .unwrap()
            .unwrap();
        assert_eq!(after.targets[1].published, before.targets[1].published);
        assert_eq!(
            std::fs::metadata(&frames[1].2).unwrap().modified().unwrap(),
            modified
        );
    }
}

#[test]
fn chapter_status_rejects_wrong_library_media_unit_and_missing_file() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let file = &f.files[0];
    let other = f
        .store
        .create_library(
            MediaKind::Tv,
            "Private",
            &[f._temp.path().join("private").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let missing = format!(
            "/api/v1/libraries/{}/items/{}/chapters/frame-status?season=1&episode=1",
            file.library_id, file.media_id
        );
        let response = f
            .app
            .clone()
            .oneshot(common::request(
                "GET",
                &missing,
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        for suffix in [
            format!("{}/items/{}", other.id, file.media_id),
            format!("{}/items/{}", file.library_id, MediaId::new()),
        ] {
            let url = format!(
                "/api/v1/libraries/{suffix}/chapters/frame-status?file_id={}&season=1&episode=1",
                file.file_id
            );
            let response = f
                .app
                .clone()
                .oneshot(common::request(
                    "GET",
                    &url,
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        assert_eq!(
            f.chapter_request(
                0,
                "GET",
                "/frame-status",
                "&episode=2",
                Value::Null,
                Some("management-secret")
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let url = format!(
            "/api/v1/libraries/{}/items/{}/chapters/frame-status?file_id={}&season=1&episode=2",
            file.library_id, file.media_id, file.file_id
        );
        let response = f
            .app
            .clone()
            .oneshot(common::request(
                "GET",
                &url,
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    });
}

#[test]
fn empty_targets_supersede_old_tracking_without_creating_a_task() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let old = f
        .store
        .current_chapter_frame_task(f.files[0].file_id)
        .unwrap()
        .unwrap();
    f.store
        .clear_cached_chapters(&f.files[0].file_id.to_string())
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let status = f
            .frame_status(
                0,
                &format!(
                    "&task_id={}&generation={}",
                    old.task.id, old.status.generation
                ),
            )
            .await;
        assert_eq!(
            status,
            json!({"task": null, "empty": true, "superseded": true})
        );
        let result = f
            .chapter_request(
                0,
                "POST",
                "/generate",
                "",
                json!({"retry_failed": false}),
                Some("management-secret"),
            )
            .await;
        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(
            common::json_body(result).await["data"],
            json!({"task":null,"empty":true})
        );
    });
}
