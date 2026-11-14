//! Temporary shared-Media Integrity ownership must defer only its own work.
use super::*;

fn shared_library(f: &Fixture) -> domain::LibraryId {
    let root = f.temp.path().join("shared");
    std::fs::create_dir(&root).unwrap();
    let library = f
        .store
        .create_library(
            MediaKind::Movie,
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
        &f.store,
        &FixtureFile {
            media_id: f.file.media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    library.id.parse().unwrap()
}

fn healthy_file(f: &Fixture) -> LibraryFile {
    let mut media = f.store.get_media(f.file.media_id).unwrap().unwrap();
    media.id = MediaId::new();
    media.title = "Healthy".into();
    f.store.insert_media(&media).unwrap();
    let path = f.file.path.with_file_name("healthy.mkv");
    std::fs::write(&path, b"healthy source").unwrap();
    publish_fixture_file(
        &f.store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap()
}

fn finish_integrity(store: &Store) {
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
}

#[test]
fn shared_media_integrity_defers_recovery_and_claim_without_blocking_healthy_work() {
    let f = Fixture::new();
    let shared = shared_library(&f);
    let healthy = healthy_file(&f);
    let blocked = f.store.ensure_chapter_frame_task(&f.task(), false).unwrap();
    let running = f
        .store
        .claim_chapter_frame_task(&blocked.task_id)
        .unwrap()
        .unwrap();
    let healthy = f
        .store
        .ensure_chapter_frame_task(
            &task(healthy.file_id, &healthy.version.source_version),
            false,
        )
        .unwrap();
    f.store
        .begin_library_integrity(shared, false, "frame-integrity", 0)
        .unwrap();

    assert_eq!(f.store.recover_chapter_frame_tasks().unwrap(), 1);
    let deferred = f
        .store
        .chapter_frame_task_snapshot(&blocked.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(deferred.status.state, ChapterFrameState::Queued);
    assert!(deferred.status.attempt > running.attempt);
    assert_eq!(deferred.targets[0].candidate, running.targets[0].candidate);
    assert!(
        !f.store
            .adopt_chapter_frame_candidates(
                &blocked.task_id,
                &deferred.status.generation,
                deferred.status.attempt,
                &[(0, deferred.targets[0].candidate.clone().unwrap())]
            )
            .unwrap()
            .applied
    );
    assert!(
        f.store
            .claim_chapter_frame_task(&blocked.task_id)
            .unwrap()
            .is_none()
    );
    let healthy_claim = f
        .store
        .claim_chapter_frame_task(&healthy.task_id)
        .unwrap()
        .unwrap();
    assert!(
        f.store
            .finish_chapter_frame_attempt(
                &healthy.task_id,
                healthy_claim.attempt,
                &[result(0, true), result(1, true)]
            )
            .unwrap()
    );

    finish_integrity(&f.store);
    let resumed = f
        .store
        .claim_chapter_frame_task(&blocked.task_id)
        .unwrap()
        .unwrap();
    assert!(resumed.attempt > deferred.status.attempt);
    assert!(
        f.store
            .finish_chapter_frame_attempt(
                &blocked.task_id,
                resumed.attempt,
                &[result(0, true), result(1, true)]
            )
            .unwrap()
    );
    assert_eq!(
        f.store
            .chapter_frame_task_status(&blocked.task_id)
            .unwrap()
            .unwrap()
            .state,
        ChapterFrameState::Completed
    );
}

#[test]
fn shared_media_integrity_defers_finish_and_accepts_a_durable_obligation() {
    let f = Fixture::new();
    let shared = shared_library(&f);
    let blocked = f.store.ensure_chapter_frame_task(&f.task(), false).unwrap();
    let running = f
        .store
        .claim_chapter_frame_task(&blocked.task_id)
        .unwrap()
        .unwrap();
    f.store
        .begin_library_integrity(shared, false, "frame-integrity", 0)
        .unwrap();
    assert!(
        !f.store
            .finish_chapter_frame_attempt(
                &blocked.task_id,
                running.attempt,
                &[result(0, true), result(1, true)]
            )
            .unwrap()
    );
    let deferred = f.store.ensure_chapter_frame_task(&f.task(), true).unwrap();
    assert_eq!(deferred.state, ChapterFrameState::Queued);
    assert_eq!(deferred.ready, 0);
    assert!(deferred.attempt > running.attempt);

    let mut next = f.task();
    next.targets = vec![(0, 1000)];
    let queued = f.store.ensure_chapter_frame_task(&next, false).unwrap();
    assert_eq!(queued.state, ChapterFrameState::Queued);
    finish_integrity(&f.store);
    assert!(
        f.store
            .claim_chapter_frame_task(&queued.task_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn missing_published_target_is_requeued_while_integrity_defers_publication() {
    let f = Fixture::new();
    let shared = shared_library(&f);
    let queued = f.store.ensure_chapter_frame_task(&f.task(), false).unwrap();
    let claim = f
        .store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(
            &queued.task_id,
            claim.attempt,
            &[result(0, true), result(1, true)],
        )
        .unwrap();
    let completed = f
        .store
        .chapter_frame_task_snapshot(&queued.task_id)
        .unwrap()
        .unwrap();
    f.store
        .begin_library_integrity(shared, false, "frame-integrity", 0)
        .unwrap();
    let repaired = f
        .store
        .repair_missing_chapter_frame_targets(
            &queued.task_id,
            &completed.status.generation,
            completed.status.attempt,
            &[(1, completed.targets[1].published.clone().unwrap())],
        )
        .unwrap();
    assert!(repaired.applied);
    assert_eq!(repaired.status.state, ChapterFrameState::Queued);
    assert_eq!(repaired.status.ready, 1);
    assert!(
        f.store
            .claim_chapter_frame_task(&queued.task_id)
            .unwrap()
            .is_none()
    );
    finish_integrity(&f.store);
    let resumed = f
        .store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.targets[0].published, completed.targets[0].published);
    assert!(resumed.targets[1].candidate.is_some());
}

#[test]
fn own_library_integrity_also_defers_a_running_obligation() {
    let f = Fixture::new();
    let queued = f.store.ensure_chapter_frame_task(&f.task(), false).unwrap();
    f.store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    f.store
        .begin_library_integrity(f.file.library_id, false, "frame-integrity", 0)
        .unwrap();
    f.store.recover_chapter_frame_tasks().unwrap();
    assert_eq!(
        f.store
            .chapter_frame_task_status(&queued.task_id)
            .unwrap()
            .unwrap()
            .state,
        ChapterFrameState::Queued
    );
    assert!(
        f.store
            .claim_chapter_frame_task(&queued.task_id)
            .unwrap()
            .is_none()
    );
    finish_integrity(&f.store);
    assert!(
        f.store
            .claim_chapter_frame_task(&queued.task_id)
            .unwrap()
            .is_some()
    );
}
