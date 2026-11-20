//! Pausing remote media admission defers STRM frame attempts without blocking local work.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api::Store;
use api::chapter_frame_service::{
    ChapterFrameRenderResult, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameService,
    ChapterFrameStaged, chapter_frame_artifact_path,
};
use api::library_lifecycle::LibraryWorkRegistry;
use domain::{Confidence, LedgerId, LibraryFile, Media, MediaId, MediaKind, QualitySource};
use library::WorkControl;
use marker::media_read;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{ChapterFrameTask, ChapterFrameTaskStatus};

const TARGETS: [(i64, i64); 2] = [(0, 10_000), (10_000, 20_000)];

struct GatedRunner {
    remote_started: AtomicUsize,
    local_started: AtomicUsize,
}

impl GatedRunner {
    fn new() -> Self {
        Self {
            remote_started: AtomicUsize::new(0),
            local_started: AtomicUsize::new(0),
        }
    }
}

impl ChapterFrameRunner for GatedRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        if request.video.extension().is_some_and(|ext| ext == "strm") {
            self.remote_started.fetch_add(1, Ordering::SeqCst);
            let _permit = media_read::acquire_media_read(&request.video, control.signal())?;
        } else {
            self.local_started.fetch_add(1, Ordering::SeqCst);
        }
        Ok(request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                let target = chapter_frame_artifact_path(
                    &request.video,
                    &request.generation,
                    *index,
                    artifact,
                );
                let staged = target.with_extension("staged.jpg");
                image::RgbImage::from_pixel(2, 2, image::Rgb([40, 80, 120]))
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

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<Store>,
    files: [LibraryFile; 3],
    tasks: [ChapterFrameTaskStatus; 2],
}

impl Fixture {
    fn new() -> Self {
        media_read::configure_remote_reads(1, Duration::ZERO);
        media_read::set_remote_reads_paused(false);

        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(temp.path().join("data")).unwrap());
        let root = temp.path().join("videos");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Frames",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Paused frames".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let files = ["remote-a.strm", "remote-b.strm", "local.mkv"].map(|name| {
            let path = root.join(name);
            let contents = if name.ends_with(".strm") {
                "https://example.invalid/video.mkv"
            } else {
                "local fixture"
            };
            std::fs::write(&path, contents).unwrap();
            publish_fixture_file(
                &store,
                &FixtureFile {
                    file_id: Some(LedgerId::new()),
                    media_id,
                    path: path.display().to_string(),
                    season: Some(1),
                    episode: Some(1),
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    ..Default::default()
                },
            )
            .unwrap()
        });
        let tasks = ["a-remote", "b-remote"].map(|id| {
            let index = if id == "a-remote" { 0 } else { 1 };
            store
                .ensure_chapter_frame_task(
                    &ChapterFrameTask {
                        id: id.into(),
                        file_id: files[index].file_id,
                        source_version: files[index].version.source_version.clone(),
                        targets: TARGETS.to_vec(),
                        targets_hash: String::new(),
                    },
                    false,
                )
                .unwrap()
        });
        for index in 0..2 {
            let status = &tasks[index];
            let claim = store
                .claim_chapter_frame_task(&status.task_id)
                .unwrap()
                .unwrap();
            let artifact = claim.targets[0].candidate.as_ref().unwrap();
            let published =
                chapter_frame_artifact_path(&files[index].path, &claim.generation, 0, artifact);
            image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]))
                .save_with_format(&published, image::ImageFormat::Jpeg)
                .unwrap();
            assert!(
                store
                    .finish_chapter_frame_attempt(
                        &claim.task.id,
                        claim.attempt,
                        &[
                            store::ChapterFrameTargetResult {
                                index: 0,
                                ready: true,
                                error: None,
                            },
                            store::ChapterFrameTargetResult {
                                index: 1,
                                ready: false,
                                error: Some("seed partial".into()),
                            },
                        ],
                    )
                    .unwrap()
            );
            store.ensure_chapter_frame_task(&claim.task, true).unwrap();
        }
        Self {
            _temp: temp,
            store,
            files,
            tasks,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        media_read::set_remote_reads_paused(false);
    }
}

async fn wait_for(predicate: impl Fn() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[tokio::test]
async fn pausing_remote_reads_requeues_strm_attempts_and_keeps_local_frames_moving() {
    let fixture = Fixture::new();
    let gate_path = fixture.files[0].path.clone();
    let external_permit =
        media_read::acquire_media_read(&gate_path, &marker::WorkControl::default())
            .expect("test permit should occupy the shared STRM gate");
    let runner = Arc::new(GatedRunner::new());
    let service = Arc::new(ChapterFrameService::with_runner(
        fixture.store.clone(),
        Arc::new(LibraryWorkRegistry::default()),
        runner.clone(),
    ));
    service.start();
    wait_for(
        || runner.remote_started.load(Ordering::SeqCst) == 2,
        "both STRM workers did not wait for remote admission",
    )
    .await;
    let running = [0, 1].map(|index| {
        fixture
            .store
            .chapter_frame_task_snapshot(&fixture.tasks[index].task_id)
            .unwrap()
            .unwrap()
    });
    assert!(
        running
            .iter()
            .all(|snapshot| snapshot.status.state == store::ChapterFrameState::Running)
    );
    let ready_before = [0, 1].map(|index| running[index].targets[0].published.clone().unwrap());
    let attempts_before = [0, 1].map(|index| running[index].status.attempt);
    for index in 0..2 {
        assert!(
            !fixture
                .store
                .defer_chapter_frame_attempt(
                    &fixture.tasks[index].task_id,
                    attempts_before[index].saturating_sub(1),
                )
                .unwrap(),
            "a stale attempt must not defer a newer running claim"
        );
    }
    tokio::time::sleep(Duration::from_millis(25)).await;

    media_read::set_remote_reads_paused(true);
    let local_task = fixture
        .store
        .ensure_chapter_frame_task(
            &ChapterFrameTask {
                id: "c-local".into(),
                file_id: fixture.files[2].file_id,
                source_version: fixture.files[2].version.source_version.clone(),
                targets: TARGETS.to_vec(),
                targets_hash: String::new(),
            },
            false,
        )
        .unwrap();
    service.notify_pending();
    wait_for(
        || runner.local_started.load(Ordering::SeqCst) > 0,
        "local task remained blocked behind paused STRM workers",
    )
    .await;
    wait_for(
        || {
            fixture
                .store
                .chapter_frame_task_snapshot(&local_task.task_id)
                .unwrap()
                .is_some_and(|snapshot| {
                    snapshot.status.state == store::ChapterFrameState::Completed
                })
        },
        "local chapter task did not complete while remote reads were paused",
    )
    .await;
    drop(external_permit);

    for index in 0..2 {
        let deferred = fixture
            .store
            .chapter_frame_task_snapshot(&fixture.tasks[index].task_id)
            .unwrap()
            .unwrap();
        assert_eq!(deferred.status.state, store::ChapterFrameState::Queued);
        assert_eq!(deferred.status.failed, 0);
        assert_eq!(deferred.status.attempt, attempts_before[index]);
        assert_eq!(
            deferred.targets[0].published.as_ref(),
            Some(&ready_before[index])
        );
        assert!(deferred.targets[0].ready);
        assert!(!deferred.targets[1].ready);
        assert!(deferred.targets[1].error.is_none());
        assert!(deferred.targets[1].candidate.is_none());
    }

    media_read::set_remote_reads_paused(false);
    wait_for(
        || {
            [0, 1].iter().all(|index| {
                fixture
                    .store
                    .chapter_frame_task_snapshot(&fixture.tasks[*index].task_id)
                    .unwrap()
                    .is_some_and(|snapshot| {
                        snapshot.status.state == store::ChapterFrameState::Completed
                    })
            })
        },
        "STRM tasks did not finish after remote reads resumed",
    )
    .await;
    assert_eq!(fixture.store.chapter_frame_task_counts().unwrap().failed, 0);
    service.shutdown().await.unwrap();
}
