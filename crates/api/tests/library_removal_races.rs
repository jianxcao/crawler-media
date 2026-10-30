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

/// 2026-10-29 E2E: deleting a Library must not wait for its in-flight scan /
/// probe / intro-outro / scrape work to finish. The removal cancels every lease
/// and deletes right away; a late publication from the still-running work is
/// refused (cancelled lease), and the store fences any bypassed write.
#[tokio::test]
async fn removal_cancels_work_and_deletes_without_waiting_for_it() {
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
        .create_library(
            domain::MediaKind::Tv,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    // Long-running work that has not noticed the cancellation yet.
    let lease = state.acquire_library_work(&library.id).unwrap();
    let removal = start_library_removal(&state, &library.id).unwrap();
    assert!(lease.control().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let current = state
                .store()
                .library_removal(&removal.operation_id)
                .unwrap()
                .unwrap();
            if current.state == "succeeded" {
                break;
            }
            assert_ne!(current.state, "failed", "{:?}", current.error);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("removal must finish while the work still holds its lease");
    assert!(state.store().get_library(&library.id).unwrap().is_none());
    assert!(
        lease.publish(|| Ok(())).is_err(),
        "late work must not publish into a removed Library"
    );
    drop(lease);
    assert!(root.is_dir(), "removal never touches the source files");
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
        .create_library(
            domain::MediaKind::Tv,
            "Stuck",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let removal = state.store().begin_library_removal(&library.id).unwrap();
    state
        .store()
        .set_removal_state(&removal.operation_id, "failed", Some("timeout"))
        .unwrap();
    api::library_lifecycle::resume_library_removals(&state).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let op = state
                .store()
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
    assert!(state.store().get_library(&library.id).unwrap().is_none());
}
