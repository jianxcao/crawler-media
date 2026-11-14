//! Explicitly ignored loopback browser fixture. No production control endpoint.
use super::*;
use api::chapter_frame_service::{
    ChapterFrameRenderResult, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameStaged,
};

struct BrowserFrames {
    control: std::path::PathBuf,
}
impl ChapterFrameRunner for BrowserFrames {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        work: &library::WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        while !self.control.join("release").exists() {
            if work.is_cancelled() {
                return Err("browser fixture stopped".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                if request
                    .video
                    .file_stem()
                    .is_some_and(|name| name == "first")
                    && request.attempt == 1
                    && *index == 1
                {
                    return ChapterFrameRenderResult {
                        index: *index,
                        staged: Err("controlled first-attempt failure".into()),
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
                    320,
                    180,
                    image::Rgb([
                        if request
                            .video
                            .file_stem()
                            .is_some_and(|name| name == "first")
                        {
                            180
                        } else {
                            40
                        },
                        40 + (*index % 5) as u8 * 30,
                        120,
                    ]),
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
struct NoChapters;
impl api::marker_resolver::ChapterProbe for NoChapters {
    fn probe(&self, _: &Path) -> Option<Vec<marker::Chapter>> {
        Some(vec![])
    }
}

fn replace_browser_source(store: &api::Store, file_id: domain::LedgerId) {
    let row = store.file_coverage_by_id(file_id).unwrap().unwrap();
    std::fs::write(
        &row.file.path,
        format!(
            "local browser replacement {:?}",
            std::time::SystemTime::now()
        ),
    )
    .unwrap();
    let observed = library::observe_source(&row.file.path).unwrap();
    let scope = store::library_write::WriteScope {
        library_id: row.file.library_id,
        config_revision: store
            .library_config_revision(&row.file.library_id.to_string())
            .unwrap(),
    };
    store
        .replace_file_source(&scope, file_id, &row.file.version, &observed, 1)
        .unwrap();
    store
        .put_cached_chapters(
            &file_id.to_string(),
            &[chapter(0, 10_000), chapter(20_000, 30_000)],
        )
        .unwrap();
}

fn prepare_browser() -> (Fixture, std::path::PathBuf) {
    let mut f = Fixture::new(&[UnitKey::new(1, 1)]);
    let control = f._temp.path().join("control");
    std::fs::create_dir(&control).unwrap();
    let second = &f.files[1];
    f.store
        .put_cached_chapters(&second.file_id.to_string(), &[])
        .unwrap();
    f.store
        .put_fixture_marker(&store::StoredFileMarker {
            file_id: second.file_id,
            version: second.version.clone(),
            unit: UnitKey::new(1, 1),
            intro_start_ms: Some(40_000),
            intro_end_ms: Some(50_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "voiceprint".into(),
            locked: true,
            updated_at: 1,
        })
        .unwrap();
    let third_path = f._temp.path().join("episodes/episode-two.mkv");
    std::fs::write(&third_path, b"local browser fixture").unwrap();
    let third = publish_fixture_file_units(
        &f.store,
        &FixtureFile {
            media_id: second.media_id,
            path: third_path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 2)],
    )
    .unwrap();
    f.store
        .put_cached_chapters(
            &third.file_id.to_string(),
            &[chapter(60_000, 70_000), chapter(70_000, 80_000)],
        )
        .unwrap();
    f.files.push(third);
    // All inputs are local and already cached; no production ffprobe/remote reader is needed.
    f.state = f
        .state
        .with_chapter_probe(Arc::new(NoChapters))
        .with_chapter_frame_runner(Arc::new(BrowserFrames {
            control: control.clone(),
        }));
    let admin = f.store.admin_user_id().unwrap();
    f.store.set_password(admin, "browser-fixture-pass").unwrap();
    f.app = api::router(f.state.clone());
    (f, control)
}

#[test]
#[ignore = "manual loopback browser fixture; launch compiled test binary outside build lock"]
fn chapter_frame_browser() {
    let (f, control) = prepare_browser();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        println!(
            "B3_BROWSER_BACKEND=http://{}",
            listener.local_addr().unwrap()
        );
        println!("B3_BROWSER_CONTROL={}", control.display());
        println!(
            "B3_BROWSER_DETAIL=/library/{}/item/{}",
            f.files[0].library_id, f.files[0].media_id
        );
        for (index, file) in f.files.iter().enumerate() {
            println!("B3_BROWSER_FILE_{index}={}", file.file_id);
        }
        println!("B3_BROWSER_LOGIN=admin (password in task-3-report.md)");
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let app = f.app.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });
        let service = f.state.chapter_frames();
        let mut started = false;
        loop {
            if control.join("stop").exists() {
                break;
            }
            if !started && control.join("start").exists() {
                service.start();
                started = true;
            }
            if control.join("replace-source").exists() {
                let store = f.state.store();
                let file = f.files[0].file_id;
                tokio::task::spawn_blocking(move || replace_browser_source(&store, file))
                    .await
                    .unwrap();
                std::fs::remove_file(control.join("replace-source")).unwrap();
                service.notify_pending();
            }
            for (command, chapters) in [
                (
                    "source-b",
                    vec![chapter(5_000, 15_000), chapter(15_000, 25_000)],
                ),
                (
                    "source-a",
                    vec![chapter(0, 10_000), chapter(20_000, 30_000)],
                ),
                ("empty", vec![]),
            ] {
                let command = control.join(command);
                if command.exists() {
                    let store = f.state.store();
                    let file = f.files[0].file_id;
                    tokio::task::spawn_blocking(move || {
                        store
                            .put_cached_chapters(&file.to_string(), &chapters)
                            .unwrap()
                    })
                    .await
                    .unwrap();
                    std::fs::remove_file(command).unwrap();
                    service.notify_pending();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        std::fs::write(control.join("release"), []).unwrap();
        service.shutdown().await.unwrap();
        let _ = shutdown.send(());
        server.await.unwrap();
    });
}
