use api::{library_lifecycle::start_library_removal, management::ApiState};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use std::sync::Arc;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("no network".into()))
    }
}

#[tokio::test]
async fn removal_waits_for_lease_before_cleaning_and_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let root = temp.path().join("extra");
    std::fs::create_dir_all(&root).unwrap();
    let library = state
        .store()
        .lock()
        .create_library(
            domain::MediaKind::Tv,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lease = state.acquire_library_work(&library.id).unwrap();
    let removal = start_library_removal(&state, &library.id).unwrap();
    assert!(lease.control().is_cancelled());
    assert!(
        state
            .store()
            .lock()
            .get_library(&library.id)
            .unwrap()
            .is_some()
    );
    drop(lease);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let current = state
                .store()
                .lock()
                .library_removal(&removal.operation_id)
                .unwrap()
                .unwrap();
            if current.state == "succeeded" {
                break;
            }
            assert_ne!(current.state, "failed", "{:?}", current.error);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(root.is_dir());
}

/// 2026-10-29 E2E: a heavy Library whose in-flight work takes longer than the old
/// fixed 30 s drain window must not flip the removal to `failed` (hidden but never
/// cleaned). The drain keeps waiting, re-cancelling, and completes once work stops.
#[tokio::test(start_paused = true)]
async fn slow_work_keeps_removal_draining_past_the_old_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let root = temp.path().join("heavy");
    std::fs::create_dir_all(&root).unwrap();
    let library = state
        .store()
        .lock()
        .create_library(
            domain::MediaKind::Tv,
            "Heavy",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lease = state.acquire_library_work(&library.id).unwrap();
    let removal = start_library_removal(&state, &library.id).unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(95)).await;
    let current = state
        .store()
        .lock()
        .library_removal(&removal.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.state, "draining", "{:?}", current.error);
    drop(lease);
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let current = state
                .store()
                .lock()
                .library_removal(&removal.operation_id)
                .unwrap()
                .unwrap();
            if current.state == "succeeded" {
                break;
            }
            assert_ne!(current.state, "failed", "{:?}", current.error);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        state
            .store()
            .lock()
            .get_library(&library.id)
            .unwrap()
            .is_none()
    );
}

/// A removal that failed earlier (e.g. before this build's drain fix) is not left
/// frozen forever: startup recovery retries it to completion.
#[tokio::test]
async fn startup_recovery_retries_failed_removal_to_completion() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        store::Store::open(temp.path().join("data")).unwrap(),
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let root = temp.path().join("stuck");
    std::fs::create_dir_all(&root).unwrap();
    let library = state
        .store()
        .lock()
        .create_library(
            domain::MediaKind::Tv,
            "Stuck",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let removal = state
        .store()
        .lock()
        .begin_library_removal(&library.id)
        .unwrap();
    state
        .store()
        .lock()
        .set_removal_state(&removal.operation_id, "failed", Some("timeout"))
        .unwrap();
    api::library_lifecycle::resume_library_removals(&state).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let op = state
                .store()
                .lock()
                .library_removal(&removal.operation_id)
                .unwrap()
                .unwrap();
            if op.state == "succeeded" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("failed removal must be retried on startup");
    assert!(
        state
            .store()
            .lock()
            .get_library(&library.id)
            .unwrap()
            .is_none()
    );
}
