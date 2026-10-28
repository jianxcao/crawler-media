use super::*;

fn artwork(f: &Fixture) -> Vec<String> {
    let parent = f.file.path.parent().unwrap();
    for name in ["poster.jpg", "fanart.jpg", "fixture-thumb.jpg"] {
        std::fs::write(parent.join(name), b"image bytes").unwrap();
    }
    std::fs::write(
        api::chapter_frame_service::chapter_frame_path(&f.file.path, 0),
        b"chapter",
    )
    .unwrap();
    let handle = f.state.store();
    let store = handle.lock();
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
        format!("/api/v1/chapters/{}/0", f.file.file_id),
        format!("/api/v1/libraries/{}/cover", f.file.library_id),
    ]
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
            let _guard = handle.lock();
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
fn concurrent_native_images_queue_instead_of_dropping_on_a_two_worker_budget() {
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
