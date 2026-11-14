//! Explicit local FFmpeg acceptance; no external service or remote source.
use super::*;

#[test]
#[ignore = "requires an installed local ffmpeg; run explicitly for renderer acceptance"]
fn local_ffmpeg_frames_are_decodable_through_the_public_image_route() {
    let mut f = Fixture::new(&[UnitKey::new(1, 1)]);
    let file = &f.files[0];
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x36:rate=2",
            "-t",
            "2",
            "-c:v",
            "mpeg4",
            "-threads",
            "1",
            "-y",
        ])
        .arg(&file.path)
        .output()
        .expect("installed local ffmpeg");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.files[0] = f.register_replacement_source(0);
    f.store
        .put_cached_chapters(
            &f.files[0].file_id.to_string(),
            &[chapter(0, 1_000), chapter(1_000, 2_000)],
        )
        .unwrap();
    f.store
        .put_cached_chapters(&f.files[1].file_id.to_string(), &[])
        .unwrap();
    f.state = f.state.with_chapter_frame_runner(Arc::new(
        api::chapter_frame_service::worker::FfmpegChapterFrameRunner::default(),
    ));
    f.app = api::router(f.state.clone());
    let service = f.state.chapter_frames();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        service.start();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let current = f
                    .store
                    .current_chapter_frame_task(f.files[0].file_id)
                    .unwrap()
                    .unwrap();
                match current.status.state {
                    store::ChapterFrameState::Completed => break,
                    store::ChapterFrameState::Failed | store::ChapterFrameState::Partial => {
                        panic!("{current:?}")
                    }
                    _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                }
            }
        })
        .await
        .unwrap();
        for item in f.chapters(UnitKey::new(1, 1)).await {
            let response = f.get(item["image_url"].as_str().unwrap()).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let image =
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();
            assert_eq!(image.width(), 960);
            assert!(image.height() > 0);
        }
        service.shutdown().await.unwrap();
    });
}
