//! Public durable frame lifecycle, using registered Library/file/source facts.
use domain::{LedgerId, LibraryFile, Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{ChapterFrameState, ChapterFrameTargetResult, ChapterFrameTask, Store};

#[path = "chapter_frame_tasks/integrity.rs"]
mod integrity;

struct Fixture {
    temp: tempfile::TempDir,
    store: Store,
    file: LibraryFile,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("media");
        std::fs::create_dir(&root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Films",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Film".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let path = root.join("film.mkv");
        std::fs::write(&path, b"registered fixture").unwrap();
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        Self { temp, store, file }
    }

    fn task(&self) -> ChapterFrameTask {
        task(self.file.file_id, &self.file.version.source_version)
    }
}

fn task(file_id: LedgerId, source_version: &str) -> ChapterFrameTask {
    ChapterFrameTask {
        id: uuid::Uuid::new_v4().to_string(),
        file_id,
        source_version: source_version.into(),
        targets: vec![(0, 10_000), (10_000, 20_000)],
        targets_hash: "caller-does-not-define-identity".into(),
    }
}

fn result(index: usize, ready: bool) -> ChapterFrameTargetResult {
    ChapterFrameTargetResult {
        index,
        ready,
        error: (!ready).then(|| "read failed".into()),
    }
}

#[test]
fn partial_frame_task_can_retry_without_losing_ready_targets() {
    let f = Fixture::new();
    let entry = f.task();
    let queued = f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let first = f
        .store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    assert!(
        f.store
            .claim_chapter_frame_task(&queued.task_id)
            .unwrap()
            .is_none()
    );
    assert!(
        f.store
            .finish_chapter_frame_attempt(
                &queued.task_id,
                first.attempt,
                &[result(0, true), result(1, false)]
            )
            .unwrap()
    );
    let partial = f
        .store
        .chapter_frame_task_status(&queued.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(partial.state, ChapterFrameState::Partial);
    assert_eq!((partial.ready, partial.failed), (1, 1));
    let published = f
        .store
        .chapter_frame_task_snapshot(&queued.task_id)
        .unwrap()
        .unwrap()
        .targets[0]
        .published
        .clone();
    assert_eq!(published, first.targets[0].candidate);
    let retried = f.store.ensure_chapter_frame_task(&entry, true).unwrap();
    assert_eq!(
        (retried.state, retried.ready, retried.failed),
        (ChapterFrameState::Queued, 1, 0)
    );
    let second = f
        .store
        .claim_chapter_frame_task(&queued.task_id)
        .unwrap()
        .unwrap();
    assert!(second.attempt > first.attempt);
    assert_eq!(second.targets[0].published, published);
    assert_eq!(second.targets[0].candidate, None);
    assert_ne!(second.targets[1].candidate, first.targets[1].candidate);
    assert!(
        !f.store
            .finish_chapter_frame_attempt(&queued.task_id, first.attempt, &[])
            .unwrap()
    );
    assert!(
        f.store
            .finish_chapter_frame_attempt(&queued.task_id, second.attempt, &[result(1, true)])
            .unwrap()
    );
    let complete = f
        .store
        .chapter_frame_task_snapshot(&queued.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (complete.status.state, complete.status.ready),
        (ChapterFrameState::Completed, 2)
    );
    assert_eq!(complete.targets[0].published, published);
    assert_eq!(
        f.store
            .ensure_chapter_frame_task(&entry, true)
            .unwrap()
            .state,
        ChapterFrameState::Completed
    );
}

#[test]
fn failed_and_missing_results_remain_observable_and_retryable() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let first = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(&entry.id, first.attempt, &[result(0, false)])
        .unwrap();
    let failed = f
        .store
        .chapter_frame_task_status(&entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (failed.state, failed.failed),
        (ChapterFrameState::Failed, 2)
    );
    assert_eq!(
        failed.targets[1].error.as_deref(),
        Some("missing_render_result")
    );
    assert_eq!(
        f.store
            .ensure_chapter_frame_task(&entry, false)
            .unwrap()
            .state,
        ChapterFrameState::Failed
    );
    f.store.ensure_chapter_frame_task(&entry, true).unwrap();
    let second = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(
            &entry.id,
            second.attempt,
            &[result(0, true), result(1, true)],
        )
        .unwrap();
    assert_eq!(f.store.chapter_frame_task_counts().unwrap().completed, 1);
}

#[test]
fn malformed_results_cannot_change_a_running_attempt() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let claim = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    let before = f
        .store
        .chapter_frame_task_snapshot(&entry.id)
        .unwrap()
        .unwrap();
    for invalid in [
        vec![result(0, true), result(0, true)],
        vec![result(2, true)],
        vec![ChapterFrameTargetResult {
            index: 0,
            ready: true,
            error: Some("contradiction".into()),
        }],
    ] {
        assert!(
            f.store
                .finish_chapter_frame_attempt(&entry.id, claim.attempt, &invalid)
                .is_err()
        );
        assert_eq!(
            f.store
                .chapter_frame_task_snapshot(&entry.id)
                .unwrap()
                .unwrap(),
            before
        );
    }
}

#[test]
fn recovery_fences_old_claims_and_adopts_only_registered_candidates() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let first = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(f.store.recover_chapter_frame_tasks().unwrap(), 1);
    let recovered = f
        .store
        .chapter_frame_task_snapshot(&entry.id)
        .unwrap()
        .unwrap();
    assert!(recovered.status.attempt > first.attempt);
    assert_eq!(recovered.targets[0].candidate, first.targets[0].candidate);
    assert!(
        !f.store
            .finish_chapter_frame_attempt(&entry.id, first.attempt, &[result(0, true)])
            .unwrap()
    );
    let candidate = first.targets[0].candidate.clone().unwrap();
    assert!(
        f.store
            .adopt_chapter_frame_candidates(
                &entry.id,
                &first.generation,
                recovered.status.attempt,
                &[(0, candidate.clone())]
            )
            .unwrap()
            .applied
    );
    assert!(
        f.store
            .retire_chapter_frame_candidates(&first)
            .unwrap()
            .is_empty(),
        "recovery still owns both the adopted artifact and retained candidate"
    );
    let second = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(second.targets[0].published, Some(candidate));
    assert_ne!(second.targets[1].candidate, first.targets[1].candidate);
    let retired = f.store.retire_chapter_frame_candidates(&first).unwrap();
    assert_eq!(
        retired,
        vec![(1, first.targets[1].candidate.clone().unwrap())]
    );
    assert!(
        !f.store
            .adopt_chapter_frame_candidates(
                &entry.id,
                &first.generation,
                recovered.status.attempt,
                &[(1, first.targets[1].candidate.clone().unwrap())]
            )
            .unwrap()
            .applied
    );
    f.store
        .finish_chapter_frame_attempt(&entry.id, second.attempt, &[result(1, true)])
        .unwrap();
    drop(f.store);
    let reopened = Store::open(f.temp.path().join("data")).unwrap();
    assert_eq!(reopened.recover_chapter_frame_tasks().unwrap(), 0);
    assert_eq!(
        reopened
            .chapter_frame_task_status(&entry.id)
            .unwrap()
            .unwrap()
            .ready,
        2
    );
}

#[test]
fn repair_only_downgrades_exact_current_published_artifacts() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let claim = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(
            &entry.id,
            claim.attempt,
            &[result(0, true), result(1, true)],
        )
        .unwrap();
    let original = f
        .store
        .chapter_frame_task_snapshot(&entry.id)
        .unwrap()
        .unwrap();
    let observed = vec![(1, original.targets[1].published.clone().unwrap())];
    let mut wrong = observed[0].1.clone();
    wrong.id = "not-the-published-artifact".into();
    assert!(
        !f.store
            .repair_missing_chapter_frame_targets(
                &entry.id,
                &claim.generation,
                claim.attempt,
                &[(1, wrong)]
            )
            .unwrap()
            .applied
    );
    assert_eq!(
        f.store
            .chapter_frame_task_snapshot(&entry.id)
            .unwrap()
            .unwrap(),
        original
    );
    let repaired = f
        .store
        .repair_missing_chapter_frame_targets(
            &entry.id,
            &claim.generation,
            claim.attempt,
            &observed,
        )
        .unwrap();
    assert!(repaired.applied);
    assert_eq!(
        (repaired.status.state, repaired.status.ready),
        (ChapterFrameState::Queued, 1)
    );
    let next = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(&entry.id, next.attempt, &[result(1, true)])
        .unwrap();
    assert!(
        !f.store
            .repair_missing_chapter_frame_targets(
                &entry.id,
                &claim.generation,
                claim.attempt,
                &observed
            )
            .unwrap()
            .applied
    );
    let after = f
        .store
        .chapter_frame_task_snapshot(&entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(after.targets[0].published, original.targets[0].published);
    assert_ne!(after.targets[1].published, original.targets[1].published);
    assert_eq!(after.status.ready, 2);
}

#[test]
fn identity_is_canonical_and_new_generation_supersedes_old_work() {
    let f = Fixture::new();
    let entry = f.task();
    let first = f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let mut duplicate = f.task();
    duplicate.targets.reverse();
    duplicate.targets.push((0, 10_000));
    assert_eq!(
        f.store
            .ensure_chapter_frame_task(&duplicate, false)
            .unwrap()
            .task_id,
        first.task_id
    );
    let old_claim = f
        .store
        .claim_chapter_frame_task(&first.task_id)
        .unwrap()
        .unwrap();
    let mut changed = f.task();
    changed.targets = vec![(1, 9_000)];
    f.store.ensure_chapter_frame_task(&changed, false).unwrap();
    assert_eq!(
        f.store
            .chapter_frame_task_status(&entry.id)
            .unwrap()
            .unwrap()
            .state,
        ChapterFrameState::Superseded
    );
    assert!(
        !f.store
            .finish_chapter_frame_attempt(&entry.id, old_claim.attempt, &[result(0, true)])
            .unwrap()
    );
    assert_eq!(
        f.store
            .ensure_chapter_frame_task(&entry, true)
            .unwrap()
            .state,
        ChapterFrameState::Superseded
    );
    assert_eq!(f.store.chapter_frame_task_counts().unwrap().terminal(), 1);
}

#[test]
fn invalid_file_source_and_targets_never_create_runnable_work() {
    let f = Fixture::new();
    let mut stale = f.task();
    stale.source_version = "stale-source".into();
    for entry in [task(LedgerId::new(), "unknown"), stale] {
        assert!(f.store.ensure_chapter_frame_task(&entry, false).is_err());
    }
    for targets in [vec![(-1, 10)], vec![(10, 10)], vec![(10, 9)]] {
        let mut entry = f.task();
        entry.targets = targets;
        assert!(f.store.ensure_chapter_frame_task(&entry, false).is_err());
    }
    assert!(f.store.pending_chapter_frame_tasks(10).unwrap().is_empty());
}

#[test]
fn empty_targets_supersede_old_work_without_creating_a_running_task() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let mut empty = f.task();
    empty.targets.clear();
    assert_eq!(
        f.store
            .ensure_chapter_frame_task(&empty, false)
            .unwrap()
            .total,
        0
    );
    assert!(f.store.pending_chapter_frame_tasks(10).unwrap().is_empty());
    assert!(
        f.store
            .claim_chapter_frame_task(&empty.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store
            .chapter_frame_task_status(&entry.id)
            .unwrap()
            .unwrap()
            .state,
        ChapterFrameState::Superseded
    );
}

#[test]
fn cancelled_tasks_and_frozen_libraries_cannot_be_revived() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let claim = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    assert!(
        f.store
            .cancel_chapter_frame_attempt(&entry.id, claim.attempt)
            .unwrap()
    );
    assert!(
        !f.store
            .finish_chapter_frame_attempt(&entry.id, claim.attempt, &[result(0, true)])
            .unwrap()
    );
    assert_eq!(
        f.store
            .ensure_chapter_frame_task(&entry, true)
            .unwrap()
            .state,
        ChapterFrameState::Cancelled
    );
    assert_eq!(f.store.recover_chapter_frame_tasks().unwrap(), 0);
    assert!(
        f.store
            .claim_chapter_frame_task(&entry.id)
            .unwrap()
            .is_none()
    );
    let mut current = f.task();
    current.targets = vec![(1, 3)];
    f.store.ensure_chapter_frame_task(&current, false).unwrap();
    f.store
        .begin_library_removal(&f.file.library_id.to_string())
        .unwrap();
    assert_eq!(
        f.store
            .chapter_frame_task_status(&current.id)
            .unwrap()
            .unwrap()
            .state,
        ChapterFrameState::Cancelled
    );
    assert!(f.store.ensure_chapter_frame_task(&current, true).is_err());
    assert!(
        f.store
            .claim_chapter_frame_task(&current.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store
            .chapter_frame_task_status(&current.id)
            .unwrap()
            .unwrap()
            .state,
        ChapterFrameState::Cancelled
    );
}

#[test]
fn pending_is_fifo_and_bounded_for_different_registered_files() {
    let f = Fixture::new();
    for index in 0..5 {
        let path = f.file.path.with_file_name(format!("file-{index}.mkv"));
        std::fs::write(&path, b"fixture").unwrap();
        let file = publish_fixture_file(
            &f.store,
            &FixtureFile {
                media_id: f.file.media_id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut entry = task(file.file_id, &file.version.source_version);
        entry.id = format!("task-{index}");
        f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    }
    let first = f.store.pending_chapter_frame_tasks(2).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["task-0", "task-1"]
    );
    assert_eq!(f.store.pending_chapter_frame_tasks(10).unwrap().len(), 5);
    assert_eq!(f.store.chapter_frame_task_counts().unwrap().queued, 5);
    let claim = f.store.claim_chapter_frame_task("task-0").unwrap().unwrap();
    f.store
        .finish_chapter_frame_attempt("task-0", claim.attempt, &[result(0, true), result(1, true)])
        .unwrap();
    let page = f.store.chapter_frame_recovery_tasks("", 2).unwrap();
    assert_eq!(
        page.iter()
            .map(|snapshot| snapshot.task.id.as_str())
            .collect::<Vec<_>>(),
        ["task-0", "task-1"]
    );
    assert_eq!(page[0].status.state, ChapterFrameState::Completed);
    let next = f
        .store
        .chapter_frame_recovery_tasks(&page[1].task.id, 2)
        .unwrap();
    assert_eq!(
        next.iter()
            .map(|snapshot| snapshot.task.id.as_str())
            .collect::<Vec<_>>(),
        ["task-2", "task-3"]
    );
}

#[test]
fn generation_covers_file_source_boundaries_and_renderer_but_not_order() {
    let file = LedgerId::new();
    let base = store::chapter_frame_generation(file, "v1", &[(0, 10), (10, 20)], 1);
    assert_eq!(
        base,
        store::chapter_frame_generation(file, "v1", &[(10, 20), (0, 10), (0, 10)], 1)
    );
    for changed in [
        store::chapter_frame_generation(LedgerId::new(), "v1", &[(0, 10), (10, 20)], 1),
        store::chapter_frame_generation(file, "v2", &[(0, 10), (10, 20)], 1),
        store::chapter_frame_generation(file, "v1", &[(0, 11), (10, 20)], 1),
        store::chapter_frame_generation(file, "v1", &[(0, 10), (10, 20)], 2),
    ] {
        assert_ne!(base, changed);
    }
}

#[test]
fn recovery_rejects_unregistered_candidate_and_changed_registered_source() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let claim = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    f.store.recover_chapter_frame_tasks().unwrap();
    let recovering = f
        .store
        .chapter_frame_task_snapshot(&entry.id)
        .unwrap()
        .unwrap();
    let mut unknown = claim.targets[0].candidate.clone().unwrap();
    unknown.id = "unregistered".into();
    assert!(
        !f.store
            .adopt_chapter_frame_candidates(
                &entry.id,
                &claim.generation,
                recovering.status.attempt,
                &[(0, unknown)]
            )
            .unwrap()
            .applied
    );
    assert_eq!(
        f.store
            .chapter_frame_task_snapshot(&entry.id)
            .unwrap()
            .unwrap(),
        recovering
    );
    std::fs::write(&f.file.path, b"registered replacement content").unwrap();
    let observation = library::observe_source(&f.file.path).unwrap();
    let scope = store::library_write::WriteScope {
        library_id: f.file.library_id,
        config_revision: f
            .store
            .library_config_revision(&f.file.library_id.to_string())
            .unwrap(),
    };
    f.store
        .replace_file_source(&scope, f.file.file_id, &f.file.version, &observation, 1)
        .unwrap();
    let rejected = f
        .store
        .adopt_chapter_frame_candidates(
            &entry.id,
            &claim.generation,
            recovering.status.attempt,
            &[(0, claim.targets[0].candidate.clone().unwrap())],
        )
        .unwrap();
    assert!(!rejected.applied);
    assert_eq!(rejected.status.state, ChapterFrameState::Superseded);
    assert!(
        f.store
            .current_chapter_frame_task(f.file.file_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn restart_keeps_published_results_while_fencing_the_unfinished_retry() {
    let f = Fixture::new();
    let entry = f.task();
    f.store.ensure_chapter_frame_task(&entry, false).unwrap();
    let first = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    f.store
        .finish_chapter_frame_attempt(
            &entry.id,
            first.attempt,
            &[result(0, true), result(1, false)],
        )
        .unwrap();
    f.store.ensure_chapter_frame_task(&entry, true).unwrap();
    let retry = f
        .store
        .claim_chapter_frame_task(&entry.id)
        .unwrap()
        .unwrap();
    drop(f.store);
    let reopened = Store::open(f.temp.path().join("data")).unwrap();
    assert_eq!(reopened.recover_chapter_frame_tasks().unwrap(), 1);
    let recovered = reopened
        .chapter_frame_task_snapshot(&entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.status.ready, 1);
    assert_eq!(recovered.targets[0].published, retry.targets[0].published);
    assert!(recovered.status.attempt > retry.attempt);
    assert!(
        !reopened
            .finish_chapter_frame_attempt(&entry.id, retry.attempt, &[result(1, true)])
            .unwrap()
    );
}
