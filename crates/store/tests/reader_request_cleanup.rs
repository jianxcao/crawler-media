//! Request scopes release snapshots and restore read policy on every exit path.
use std::{
    num::NonZeroUsize,
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use store::{Db, LibraryReadError, QueryBudget, Store, StoreError};

fn fixture() -> (tempfile::TempDir, Store) {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .configure_reader_pool_size(Db::Library, NonZeroUsize::new(1).unwrap())
        .unwrap();
    (dir, store)
}

fn assert_reader_recovered(store: &Store) {
    assert_eq!(store.reader_pool(Db::Library).available(), 1);
    store
        .with_read_snapshot(Duration::from_secs(1), || {
            assert!(!store.list_libraries().unwrap().is_empty());
            assert_eq!(store.reader_pool(Db::Library).available(), 0);
        })
        .unwrap();
    assert_eq!(store.reader_pool(Db::Library).available(), 1);
}

#[test]
fn panic_releases_the_active_snapshot_and_restores_request_policy() {
    let (_dir, store) = fixture();
    let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = store.with_read_snapshot(Duration::from_secs(1), || {
            store.list_libraries().unwrap();
            assert_eq!(store.reader_pool(Db::Library).available(), 0);
            panic!("request fixture panic");
        });
    }));
    assert!(panic.is_err());
    assert_reader_recovered(&store);
}

#[test]
fn sql_failure_releases_the_active_snapshot_before_returning_the_error() {
    let (_dir, store) = fixture();
    let result = store.with_read_snapshot(Duration::from_secs(1), || {
        store.list_libraries().unwrap();
        store.with_read(Db::Library, |conn| {
            conn.query_row("SELECT value FROM missing_request_fixture", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(StoreError::from)
        })
    });
    assert!(
        matches!(result.unwrap(), Err(StoreError::Sqlite(_))),
        "the original SQL failure must survive cleanup"
    );
    assert_reader_recovered(&store);
}

#[test]
fn cancellation_after_admission_releases_reader_and_cancellation_policy() {
    let (_dir, store) = fixture();
    let cancelled = Arc::new(AtomicBool::new(false));
    let budget = QueryBudget {
        deadline: Instant::now() + Duration::from_secs(10),
        cancelled: cancelled.clone(),
    };
    let result = store.with_query_budget(&budget, |store| {
        store
            .with_read_snapshot(Duration::from_secs(1), || {
                store.list_libraries().unwrap();
                cancelled.store(true, Ordering::Release);
                assert!(matches!(
                    store.list_libraries(),
                    Err(StoreError::ReadCancelled)
                ));
            })
            .map_err(LibraryReadError::Store)
    });
    assert!(matches!(result, Err(LibraryReadError::Cancelled)));
    assert_reader_recovered(&store);
}

#[test]
fn nested_expired_deadline_cleans_up_and_does_not_poison_the_outer_scope() {
    let (_dir, store) = fixture();
    store
        .with_read_snapshot(Duration::from_secs(1), || {
            let before = store.list_libraries().unwrap();
            let expired = store.with_read_snapshot(Duration::ZERO, || store.list_libraries());
            assert!(matches!(expired, Err(StoreError::ReadDeadline)));
            assert_eq!(store.list_libraries().unwrap(), before);
            assert_eq!(store.reader_pool(Db::Library).available(), 0);
        })
        .unwrap();
    assert_reader_recovered(&store);
}

#[test]
fn an_expired_request_releases_its_already_admitted_reader() {
    let (_dir, store) = fixture();
    let result = store.with_read_snapshot(Duration::from_secs(1), || {
        store.list_libraries().unwrap();
        assert_eq!(store.reader_pool(Db::Library).available(), 0);
        std::thread::sleep(Duration::from_secs(1));
        assert!(matches!(
            store.list_libraries(),
            Err(StoreError::ReadDeadline)
        ));
    });
    assert!(matches!(result, Err(StoreError::ReadDeadline)));
    assert_reader_recovered(&store);
}
