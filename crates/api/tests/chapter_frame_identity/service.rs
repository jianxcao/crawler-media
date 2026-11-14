//! HTTP consumes the actual service's per-target publication and retry results.
use super::*;
use api::chapter_frame_service::{
    ChapterFrameRenderResult, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameStaged,
};

struct PartialRunner;
impl ChapterFrameRunner for PartialRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        _: &library::WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        Ok(request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                if request.attempt == 1 && *index == 1 {
                    return ChapterFrameRenderResult {
                        index: *index,
                        staged: Err("controlled read failure".into()),
                    };
                }
                let target = chapter_frame_artifact_path(
                    &request.video,
                    &request.generation,
                    *index,
                    artifact,
                );
                let staged = target.with_extension("staging.jpg");
                image::RgbImage::from_pixel(
                    2,
                    2,
                    image::Rgb([*index as u8 * 100, request.attempt as u8 * 30, 80]),
                )
                .save_with_format(&staged, image::ImageFormat::Jpeg)
                .unwrap();
                ChapterFrameRenderResult {
                    index: *index,
                    staged: Ok(ChapterFrameStaged { staged, target }),
                }
            })
            .collect())
    }
}

pub(super) async fn wait_state(
    f: &Fixture,
    state: store::ChapterFrameState,
) -> store::ChapterFrameTaskSnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let current = f
                .store
                .current_chapter_frame_task(f.files[0].file_id)
                .unwrap()
                .unwrap();
            if current.status.state == state {
                break current;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("chapter service failed to reach the expected state")
}

async fn generate(f: &Fixture) {
    let file = &f.files[0];
    let uri = format!(
        "/api/v1/libraries/{}/items/{}/chapters/generate?file_id={}&season=1&episode=1",
        file.library_id, file.media_id, file.file_id
    );
    let response = f
        .app
        .clone()
        .oneshot(common::request(
            "POST",
            &uri,
            Some("management-secret"),
            serde_json::json!({"retry_failed": true}),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::json_body(response).await
    );
}

#[test]
fn partial_images_are_public_and_explicit_retry_repairs_only_missing_artifacts() {
    let mut f = Fixture::new(&[UnitKey::new(1, 1)]);
    f.state = f.state.with_chapter_frame_runner(Arc::new(PartialRunner));
    let service = f.state.chapter_frames();
    f.app = api::router(f.state.clone());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        service.start();
        let partial = wait_state(&f, store::ChapterFrameState::Partial).await;
        assert_eq!((partial.status.ready, partial.status.failed), (1, 1));
        let items = f.chapters(UnitKey::new(1, 1)).await;
        let first = items
            .iter()
            .find(|chapter| chapter["start_ms"] == 0)
            .unwrap()["image_url"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(f.get(&first).await.status(), StatusCode::OK);
        assert!(
            items
                .iter()
                .filter(|chapter| chapter["start_ms"] != 0)
                .all(|chapter| chapter["image_url"].is_null())
        );
        let artifact = partial.targets[0].published.as_ref().unwrap();
        let path =
            chapter_frame_artifact_path(&f.files[0].path, &partial.status.generation, 0, artifact);
        let bytes = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let unchanged = f
            .chapter_request(
                0,
                "POST",
                "/generate",
                "",
                serde_json::json!({"retry_failed":false}),
                Some("management-secret"),
            )
            .await;
        assert_eq!(unchanged.status(), StatusCode::OK);
        let unchanged = common::json_body(unchanged).await;
        assert_eq!(unchanged["data"]["task"]["state"], "partial");
        assert_eq!(unchanged["data"]["task"]["attempt"], partial.status.attempt);
        generate(&f).await;
        let completed = wait_state(&f, store::ChapterFrameState::Completed).await;
        assert_eq!(completed.status.ready, 2);
        assert_eq!(completed.targets[0].published, partial.targets[0].published);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        for item in f.chapters(UnitKey::new(1, 1)).await {
            assert_eq!(
                f.get(item["image_url"].as_str().unwrap()).await.status(),
                StatusCode::OK
            );
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(f.get(&first).await.status(), StatusCode::NOT_FOUND);
        generate(&f).await;
        let repaired = wait_state(&f, store::ChapterFrameState::Completed).await;
        assert_ne!(
            repaired.targets[0].published,
            completed.targets[0].published
        );
        assert_eq!(
            repaired.targets[1].published,
            completed.targets[1].published
        );
        assert_eq!(f.get(&first).await.status(), StatusCode::NOT_FOUND);
        service.shutdown().await.unwrap();
    });
}
