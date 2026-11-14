//! Partial retries, immutable recovery and exact missing-image repair.
use super::*;

struct PartialRunner;

impl ChapterFrameRunner for PartialRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        _: &WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        Ok(request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                if *index == 1 && request.attempt == 1 {
                    return ChapterFrameRenderResult {
                        index: *index,
                        staged: Err("read failed".into()),
                    };
                }
                let target = chapter_frame_artifact_path(
                    &request.video,
                    &request.generation,
                    *index,
                    artifact,
                );
                let staged = target.with_extension("staging.jpg");
                write_frame(&staged);
                ChapterFrameRenderResult {
                    index: *index,
                    staged: Ok(ChapterFrameStaged { staged, target }),
                }
            })
            .collect())
    }
}

pub(super) async fn wait_state(
    store: &Store,
    file_id: LedgerId,
    state: store::ChapterFrameState,
) -> store::ChapterFrameTaskSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(snapshot) = store.current_chapter_frame_task(file_id).unwrap()
                && snapshot.status.state == state
            {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "frame task did not reach {state:?}: current={:?}, counts={:?}",
            store.current_chapter_frame_task(file_id).unwrap(),
            store.chapter_frame_task_counts().unwrap()
        )
    })
}

#[tokio::test]
async fn partial_retry_preserves_published_artifact_and_only_renders_pending_target() {
    use store::ChapterFrameState;
    let fixture = Fixture::new();
    let service = Arc::new(ChapterFrameService::with_runner(
        fixture.store.clone(),
        fixture.registry.clone(),
        Arc::new(PartialRunner),
    ));
    let file = &fixture.rows[0];
    service
        .enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
        .unwrap();
    service.start();
    let partial = wait_state(&fixture.store, file.file_id, ChapterFrameState::Partial).await;
    assert_eq!((partial.status.ready, partial.status.failed), (1, 1));
    let artifact = partial.targets[0].published.clone().unwrap();
    let path = chapter_frame_artifact_path(&file.path, &partial.status.generation, 0, &artifact);
    let bytes = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert!(
        service
            .enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
            .unwrap()
    );
    let complete = wait_state(&fixture.store, file.file_id, ChapterFrameState::Completed).await;
    assert_eq!(complete.status.ready, 2);
    assert_eq!(complete.targets[0].published.as_ref(), Some(&artifact));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    service.shutdown().await.unwrap();
}

struct UnacknowledgedRunner {
    gate: Arc<Gate>,
}

impl ChapterFrameRunner for UnacknowledgedRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        _: &WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        let results = request
            .candidates
            .iter()
            .map(|(index, artifact)| {
                let target = chapter_frame_artifact_path(
                    &request.video,
                    &request.generation,
                    *index,
                    artifact,
                );
                // Simulate process interruption after immutable file publication,
                // before its result is acknowledged in the Store.
                write_frame(&target);
                let staged = target.with_extension("staging.jpg");
                write_frame(&staged);
                ChapterFrameRenderResult {
                    index: *index,
                    staged: Ok(ChapterFrameStaged { staged, target }),
                }
            })
            .collect();
        self.gate.enter();
        Ok(results)
    }
}

struct ReleaseGate(Arc<Gate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test]
async fn late_worker_cleanup_preserves_artifacts_adopted_by_recovery() {
    use store::ChapterFrameState;
    let fixture = Fixture::new();
    let gate = Arc::new(Gate::new());
    let _release = ReleaseGate(gate.clone());
    let old = Arc::new(ChapterFrameService::with_runner(
        fixture.store.clone(),
        fixture.registry.clone(),
        Arc::new(UnacknowledgedRunner { gate: gate.clone() }),
    ));
    let file = &fixture.rows[0];
    old.enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
        .unwrap();
    old.start();
    tokio::time::timeout(Duration::from_secs(10), async {
        while gate.entered() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let running = fixture
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    let candidate = running.targets[0].candidate.clone().unwrap();
    let path = chapter_frame_artifact_path(&file.path, &running.status.generation, 0, &candidate);
    let bytes = std::fs::read(&path).unwrap();
    let recovered = Arc::new(ChapterFrameService::with_runner(
        fixture.store.clone(),
        fixture.registry.clone(),
        Arc::new(PanicRunner),
    ));
    recovered.start();
    let adopted = wait_state(&fixture.store, file.file_id, ChapterFrameState::Completed).await;
    assert!(adopted.status.attempt > running.status.attempt);
    assert_eq!(adopted.targets[0].published.as_ref(), Some(&candidate));
    gate.release();
    tokio::time::timeout(Duration::from_secs(10), async {
        while old.pending_memory() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        fixture
            .store
            .current_chapter_frame_task(file.file_id)
            .unwrap()
            .unwrap(),
        adopted
    );
    old.shutdown().await.unwrap();
    recovered.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_or_invalid_published_images_are_repaired_without_rewriting_good_images() {
    use store::ChapterFrameState;
    let fixture = Fixture::new();
    fixture.gate.release();
    let file = &fixture.rows[0];
    fixture
        .service
        .enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
        .unwrap();
    fixture.service.start();
    let mut snapshot = wait_state(&fixture.store, file.file_id, ChapterFrameState::Completed).await;
    let good = snapshot.targets[0].published.clone().unwrap();
    let good_path = chapter_frame_artifact_path(&file.path, &snapshot.status.generation, 0, &good);
    let good_bytes = std::fs::read(&good_path).unwrap();
    let good_modified = std::fs::metadata(&good_path).unwrap().modified().unwrap();
    for invalid in [None, Some(b"".as_slice()), Some(b"not a JPEG".as_slice())] {
        let broken = chapter_frame_artifact_path(
            &file.path,
            &snapshot.status.generation,
            1,
            snapshot.targets[1].published.as_ref().unwrap(),
        );
        match invalid {
            None => std::fs::remove_file(&broken).unwrap(),
            Some(bytes) => std::fs::write(&broken, bytes).unwrap(),
        }
        let old_attempt = snapshot.status.attempt;
        fixture
            .service
            .enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
            .unwrap();
        snapshot = wait_state(&fixture.store, file.file_id, ChapterFrameState::Completed).await;
        assert!(snapshot.status.attempt > old_attempt);
        let repaired = chapter_frame_artifact_path(
            &file.path,
            &snapshot.status.generation,
            1,
            snapshot.targets[1].published.as_ref().unwrap(),
        );
        assert!(image::load_from_memory(&std::fs::read(repaired).unwrap()).is_ok());
        assert_eq!(snapshot.targets[0].published.as_ref(), Some(&good));
        assert_eq!(std::fs::read(&good_path).unwrap(), good_bytes);
        assert_eq!(
            std::fs::metadata(&good_path).unwrap().modified().unwrap(),
            good_modified
        );
    }
    fixture.service.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_repairs_a_missing_published_target_before_rerendering() {
    use store::ChapterFrameState;
    let fixture = Fixture::new();
    fixture.gate.release();
    let file = &fixture.rows[0];
    fixture
        .service
        .enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
        .unwrap();
    fixture.service.start();
    let before = wait_state(&fixture.store, file.file_id, ChapterFrameState::Completed).await;
    fixture.service.shutdown().await.unwrap();
    let path = chapter_frame_artifact_path(
        &file.path,
        &before.status.generation,
        1,
        before.targets[1].published.as_ref().unwrap(),
    );
    std::fs::remove_file(&path).unwrap();
    fixture.service.start();
    let after = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = fixture
                .store
                .current_chapter_frame_task(file.file_id)
                .unwrap()
                .unwrap();
            if current.status.state == ChapterFrameState::Completed
                && current.status.attempt > before.status.attempt
            {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("restart did not repair missing image");
    assert_eq!(after.targets[0].published, before.targets[0].published);
    assert_ne!(after.targets[1].published, before.targets[1].published);
    fixture.service.shutdown().await.unwrap();
}

#[derive(Default)]
struct CancellationRunner {
    started: std::sync::atomic::AtomicBool,
    stopped: std::sync::atomic::AtomicBool,
    force_release: std::sync::atomic::AtomicBool,
}

impl ChapterFrameRunner for CancellationRunner {
    fn render(
        &self,
        _: &ChapterFrameRequest,
        control: &WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        self.started.store(true, Ordering::SeqCst);
        while !control.is_cancelled() && !self.force_release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.stopped.store(true, Ordering::SeqCst);
        Err("render cancelled".into())
    }
}

struct ReleaseCancellationRunner(Arc<CancellationRunner>);

impl Drop for ReleaseCancellationRunner {
    fn drop(&mut self) {
        self.0.force_release.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn aborted_service_retains_render_lease_until_library_freeze_stops_it() {
    let fixture = Fixture::new();
    let runner = Arc::new(CancellationRunner::default());
    let _release = ReleaseCancellationRunner(runner.clone());
    let service = Arc::new(ChapterFrameService::with_runner(
        fixture.store.clone(),
        fixture.registry.clone(),
        runner.clone(),
    ));
    let file = &fixture.rows[0];
    service
        .enqueue_ledger(&file.file_id.to_string(), TARGETS.to_vec())
        .unwrap();
    service.start();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !runner.started.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancellable renderer did not start");
    let task = fixture
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();

    service.abort();
    let library_id = file.library_id.to_string();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            fixture.registry.wait_idle(&library_id)
        )
        .await
        .is_err(),
        "aborting async dispatch must not unregister an active blocking render"
    );
    fixture
        .registry
        .freeze(&fixture.store, &library_id)
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.registry.wait_idle(&library_id),
    )
    .await
    .expect("Library freeze did not stop the detached renderer");
    assert!(runner.stopped.load(Ordering::SeqCst));
    assert_eq!(
        fixture
            .store
            .chapter_frame_task_status(&task.task.id)
            .unwrap()
            .unwrap()
            .state,
        store::ChapterFrameState::Cancelled
    );
}
