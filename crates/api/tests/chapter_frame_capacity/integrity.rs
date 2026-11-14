//! Public startup/dispatch behavior under another Library's shared-Media barrier.
use super::recovery::wait_state;
use super::*;
use store::{ChapterFrameState, ChapterFrameTask};

fn shared_library(fixture: &Fixture) -> domain::LibraryId {
    let root = fixture._temp.path().join("shared");
    std::fs::create_dir(&root).unwrap();
    let library = fixture
        .store
        .create_library(
            MediaKind::Tv,
            "Shared",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("shared.mkv");
    std::fs::write(&path, b"shared source").unwrap();
    publish_fixture_file(
        &fixture.store,
        &FixtureFile {
            media_id: fixture.rows[0].media_id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    library.id.parse().unwrap()
}

fn healthy_file(fixture: &Fixture, name: &str) -> LibraryFile {
    let mut media = fixture
        .store
        .get_media(fixture.rows[0].media_id)
        .unwrap()
        .unwrap();
    media.id = MediaId::new();
    media.title = name.into();
    fixture.store.insert_media(&media).unwrap();
    let path = fixture.rows[0].path.with_file_name(format!("{name}.mkv"));
    std::fs::write(&path, b"healthy source").unwrap();
    publish_fixture_file(
        &fixture.store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap()
}

fn enqueue(fixture: &Fixture, file: &LibraryFile, id: String) -> store::ChapterFrameTaskStatus {
    fixture
        .store
        .ensure_chapter_frame_task(
            &ChapterFrameTask {
                id,
                file_id: file.file_id,
                source_version: file.version.source_version.clone(),
                targets: TARGETS.to_vec(),
                targets_hash: String::new(),
            },
            false,
        )
        .unwrap()
}

async fn release_integrity(store: Arc<Store>) {
    tokio::task::spawn_blocking(move || {
        for step in 1..1000 {
            let status = store
                .advance_library_operation("frame-integrity", step, 100)
                .unwrap();
            if status.state == "succeeded" {
                return;
            }
            assert_eq!(status.state, "applying", "{status:?}");
        }
        panic!("Integrity did not release its owner");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn startup_defers_shared_media_and_automatically_resumes_after_integrity() {
    let fixture = Fixture::new();
    fixture.gate.release();
    let shared = shared_library(&fixture);
    let healthy = healthy_file(&fixture, "healthy");
    let blocked = enqueue(&fixture, &fixture.rows[0], "blocked".into());
    let running = fixture
        .store
        .claim_chapter_frame_task(&blocked.task_id)
        .unwrap()
        .unwrap();
    enqueue(&fixture, &healthy, "healthy".into());
    fixture
        .store
        .begin_library_integrity(shared, false, "frame-integrity", 0)
        .unwrap();
    fixture.service.start();

    wait_state(
        &fixture.store,
        healthy.file_id,
        ChapterFrameState::Completed,
    )
    .await;
    let deferred = fixture
        .store
        .chapter_frame_task_status(&blocked.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(deferred.state, ChapterFrameState::Queued);
    assert!(deferred.attempt > running.attempt);
    release_integrity(fixture.store.clone()).await;
    let resumed = wait_state(
        &fixture.store,
        fixture.rows[0].file_id,
        ChapterFrameState::Completed,
    )
    .await;
    assert!(resumed.status.attempt > deferred.attempt);
    fixture.service.shutdown().await.unwrap();
}

#[tokio::test]
async fn dispatch_skips_blocked_fifo_pages_and_resumes_them_after_integrity() {
    let fixture = Fixture::new();
    fixture.gate.release();
    let shared = shared_library(&fixture);
    let warmup = healthy_file(&fixture, "warmup");
    let healthy = healthy_file(&fixture, "healthy");
    fixture
        .service
        .enqueue_ledger(&warmup.file_id.to_string(), TARGETS.to_vec())
        .unwrap();
    fixture.service.start();
    wait_state(&fixture.store, warmup.file_id, ChapterFrameState::Completed).await;
    for (index, file) in fixture.rows[..199].iter().enumerate() {
        enqueue(&fixture, file, format!("blocked-{index:03}"));
    }
    enqueue(&fixture, &healthy, "healthy-last".into());
    fixture
        .store
        .begin_library_integrity(shared, false, "frame-integrity", 0)
        .unwrap();
    fixture.service.dispatch_once().await.unwrap();

    wait_state(
        &fixture.store,
        healthy.file_id,
        ChapterFrameState::Completed,
    )
    .await;
    assert_eq!(fixture.counts().queued, 199);
    assert_eq!(fixture.counts().cancelled, 0);
    assert!(fixture.service.pending_memory() <= DISPATCH_WINDOW);
    release_integrity(fixture.store.clone()).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.counts().completed != 201 {
            assert!(fixture.service.pending_memory() <= DISPATCH_WINDOW);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deferred tasks did not resume automatically");
    assert!(fixture.runner.max_active() <= MAX_CHAPTER_PROCESSES);
    fixture.service.shutdown().await.unwrap();
}

fn retry_with_valid_candidate_and_missing_published_image(
    fixture: &Fixture,
) -> store::ChapterFrameClaim {
    let file = &fixture.rows[0];
    let queued = enqueue(fixture, file, "mixed-recovery".into());
    let first = fixture
        .store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    let published = chapter_frame_artifact_path(
        &file.path,
        &first.generation,
        0,
        first.targets[0].candidate.as_ref().unwrap(),
    );
    write_frame(&published);
    fixture
        .store
        .finish_chapter_frame_attempt(
            &queued.task_id,
            first.attempt,
            &[
                store::ChapterFrameTargetResult {
                    index: 0,
                    ready: true,
                    error: None,
                },
                store::ChapterFrameTargetResult {
                    index: 1,
                    ready: false,
                    error: Some("read failed".into()),
                },
            ],
        )
        .unwrap();
    let partial = fixture
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(partial.status.state, ChapterFrameState::Partial);
    fixture
        .store
        .ensure_chapter_frame_task(&partial.task, true)
        .unwrap();
    let retry = fixture
        .store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    write_frame(&chapter_frame_artifact_path(
        &file.path,
        &retry.generation,
        1,
        retry.targets[1].candidate.as_ref().unwrap(),
    ));
    std::fs::remove_file(published).unwrap();
    retry
}

#[tokio::test]
async fn deferred_candidate_adoption_still_repairs_a_missing_published_image() {
    let fixture = Fixture::new();
    fixture.gate.release();
    let shared = shared_library(&fixture);
    let healthy = healthy_file(&fixture, "healthy");
    let retry = retry_with_valid_candidate_and_missing_published_image(&fixture);
    enqueue(&fixture, &healthy, "healthy".into());
    fixture
        .store
        .begin_library_integrity(shared, false, "frame-integrity", 0)
        .unwrap();
    fixture.service.start();
    wait_state(
        &fixture.store,
        healthy.file_id,
        ChapterFrameState::Completed,
    )
    .await;
    let deferred = fixture
        .store
        .chapter_frame_task_snapshot(&retry.task.id)
        .unwrap()
        .unwrap();
    assert_eq!(deferred.status.state, ChapterFrameState::Queued);
    assert!(deferred.status.attempt > retry.attempt);

    release_integrity(fixture.store.clone()).await;
    let completed = wait_state(
        &fixture.store,
        fixture.rows[0].file_id,
        ChapterFrameState::Completed,
    )
    .await;
    assert_eq!((completed.status.ready, completed.status.failed), (2, 0));
    for target in &completed.targets {
        assert!(target.ready);
        let path = chapter_frame_artifact_path(
            &fixture.rows[0].path,
            &completed.status.generation,
            target.index,
            target.published.as_ref().unwrap(),
        );
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "completed target{} has no published image at {}: {error}",
                target.index,
                path.display()
            )
        });
        assert!(image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).is_ok());
    }
    assert_ne!(completed.targets[0].published, retry.targets[0].published);
    fixture.service.shutdown().await.unwrap();
}
