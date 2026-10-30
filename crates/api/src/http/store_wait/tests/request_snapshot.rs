//! The async API wrapper binds one reader for all work in its blocking stage.
use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use super::store_read_handle;

fn fixture() -> (tempfile::TempDir, Arc<crate::Store>) {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Arc::new(crate::Store::open(dir.path()).unwrap());
    store
        .configure_reader_pool_size(store::Db::Library, NonZeroUsize::new(1).unwrap())
        .unwrap();
    (dir, store)
}

#[tokio::test]
async fn api_read_stage_checks_out_once_for_repeated_store_methods() {
    let (_dir, store) = fixture();
    let (library, stats) = store_read_handle(store.clone(), Duration::from_secs(5), |store| {
        store.with_stats(store::stats::StatsScope::Library, || {
            let library = store
                .default_library(domain::MediaKind::Movie)
                .unwrap()
                .unwrap();
            assert_eq!(store.get_library(&library.id).unwrap().unwrap(), library);
            assert!(store.list_libraries().unwrap().contains(&library));
            library
        })
    })
    .await
    .unwrap();
    assert_eq!(library.kind, domain::MediaKind::Movie);
    assert_eq!(stats.reader_checkouts, 1, "API stage must bind one reader");
    assert_eq!(store.reader_pool(store::Db::Library).available(), 1);
}

#[tokio::test]
async fn api_read_stage_keeps_snapshot_when_another_writer_commits_between_reads() {
    let (dir, store) = fixture();
    let writer = crate::Store::open(dir.path()).unwrap();
    let old_count = store_read_handle(store.clone(), Duration::from_secs(5), move |store| {
        let before = store.list_libraries().unwrap();
        std::thread::spawn(move || {
            writer
                .create_library(
                    domain::MediaKind::Movie,
                    "Committed while API reader is active",
                    &["/api-request-snapshot/new"],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(
            store.list_libraries().unwrap(),
            before,
            "an API read stage must observe one snapshot"
        );
        before.len()
    })
    .await
    .unwrap();
    let new_count = store_read_handle(store.clone(), Duration::from_secs(5), |store| {
        store.list_libraries().unwrap().len()
    })
    .await
    .unwrap();
    assert_eq!(new_count, old_count + 1);
    assert_eq!(store.reader_pool(store::Db::Library).available(), 1);
}
