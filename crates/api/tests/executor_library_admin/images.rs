use super::*;

fn artwork(f: &Fixture) -> Vec<String> {
    let parent = f.file.path.parent().unwrap();
    for name in ["poster.jpg", "fanart.jpg", "fixture-thumb.jpg"] {
        std::fs::write(parent.join(name), b"image bytes").unwrap();
    }
    let handle = f.state.store();
    let store = handle.as_ref();
    let library = store
        .get_library(&f.file.library_id.to_string())
        .unwrap()
        .unwrap();
    store
        .compare_set_library_cover(
            &library.id,
            library.cover_revision,
            Some(&parent.join("poster.jpg").display().to_string()),
            "manual",
        )
        .unwrap();
    vec![
        format!("/api/v1/posters/{}", f.file.file_id),
        format!("/api/v1/fanart/{}", f.file.file_id),
        format!("/api/v1/stills/{}", f.file.file_id),
        chapter_image(f, store),
        format!("/api/v1/libraries/{}/cover", f.file.library_id),
    ]
}

fn chapter_image(f: &Fixture, store: &Store) -> String {
    store
        .put_cached_chapters(
            &f.file.file_id.to_string(),
            &[library::ChapterMarker {
                start_ms: 0,
                end_ms: 10_000,
                title: None,
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    let current = store
        .current_chapter_frame_task(f.file.file_id)
        .unwrap()
        .unwrap();
    let claim = store
        .claim_chapter_frame_task(&current.task.id)
        .unwrap()
        .unwrap();
    let artifact = claim.targets[0].candidate.as_ref().unwrap();
    let path = api::chapter_frame_service::chapter_frame_artifact_path(
        &f.file.path,
        &claim.generation,
        0,
        artifact,
    );
    image::RgbImage::from_pixel(2, 2, image::Rgb([30, 60, 90]))
        .save_with_format(path, image::ImageFormat::Jpeg)
        .unwrap();
    assert!(
        store
            .finish_chapter_frame_attempt(
                &claim.task.id,
                claim.attempt,
                &[store::ChapterFrameTargetResult {
                    index: 0,
                    ready: true,
                    error: None
                },]
            )
            .unwrap()
    );
    format!(
        "/api/v1/chapters/{}/0?generation={}&artifact={}-{}",
        f.file.file_id, claim.generation, artifact.attempt, artifact.id
    )
}

fn image_request(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[test]
fn native_artwork_routes_remain_public_and_keep_executor_responsive() {
    let f = Fixture::new();
    for uri in artwork(&f) {
        let handle = f.state.store();
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard =
                crate::writer_hold::WriterHold::acquire(handle.clone(), store::Db::Library);
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(
                router(f.state.clone()).oneshot(image_request(&uri)),
                async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    started.elapsed()
                }
            )
        });
        holder.join().unwrap();
        assert_eq!(response.unwrap().status(), StatusCode::OK, "{uri}");
        assert!(
            elapsed < Duration::from_millis(200),
            "{uri} parked timer for {elapsed:?}"
        );
    }
}

struct SlowImageFiles;
impl api::file_stat_read::FileStatRead for SlowImageFiles {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        std::thread::sleep(Duration::from_millis(40));
        path.is_file()
    }
}

#[test]
fn concurrent_native_images_queue_within_the_dedicated_read_budget() {
    let mut f = Fixture::new();
    let uris = artwork(&f);
    f.state = f.state.with_file_stat_read(Arc::new(SlowImageFiles));
    runtime().block_on(async {
        let mut requests = tokio::task::JoinSet::new();
        for i in 0..24 {
            let app = router(f.state.clone());
            let uri = uris[i % 3].clone();
            requests.spawn(async move { app.oneshot(image_request(&uri)).await.unwrap().status() });
        }
        while let Some(status) = requests.join_next().await {
            assert_eq!(status.unwrap(), StatusCode::OK);
        }
    });
    assert_eq!(f.state.fs_work.available_permits(), 2);
}

#[test]
fn missing_episode_still_does_not_serve_the_backdrop() {
    let f = Fixture::new();
    std::fs::write(
        f.file.path.parent().unwrap().join("fanart.jpg"),
        b"backdrop",
    )
    .unwrap();
    let uri = format!("/api/v1/stills/{}", f.file.file_id);
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(image_request(&uri)))
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

struct StalledImageFiles;
impl api::file_stat_read::FileStatRead for StalledImageFiles {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        std::thread::sleep(Duration::from_secs(2));
        path.is_file()
    }
}

#[test]
fn slow_native_sidecar_read_has_a_bounded_response_time() {
    let mut f = Fixture::new();
    let uris = artwork(&f);
    f.state = f.state.with_file_stat_read(Arc::new(StalledImageFiles));
    let response = runtime().block_on(async {
        tokio::time::timeout(
            Duration::from_millis(1500),
            router(f.state.clone()).oneshot(image_request(&uris[0])),
        )
        .await
    });
    assert_eq!(
        response
            .expect("slow sidecar reads must free the browser connection within the image deadline")
            .unwrap()
            .status(),
        StatusCode::GATEWAY_TIMEOUT
    );
}

#[test]
fn cached_native_images_do_not_wait_for_maintenance_filesystem_work() {
    let f = Fixture::new();
    let uris = artwork(&f);
    let one = f.state.fs_work.try_reserve().unwrap();
    let two = f.state.fs_work.try_reserve().unwrap();
    runtime().block_on(async {
        for uri in uris {
            let response = tokio::time::timeout(
                Duration::from_millis(500),
                router(f.state.clone()).oneshot(image_request(&uri)),
            )
            .await;
            assert_eq!(
                response
                    .expect("cached images must not queue behind directory scans")
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
    });
    drop((one, two));
}
