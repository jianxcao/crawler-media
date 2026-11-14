//! Async-admitted readers remain the only connection for synchronous Store work.
use std::{num::NonZeroUsize, panic::AssertUnwindSafe};
use store::{Db, Store, StoreError};

fn one_reader(store: &Store) {
    store
        .configure_reader_pool_size(Db::Library, NonZeroUsize::new(1).unwrap())
        .unwrap();
}

#[test]
fn scoped_reader_reuses_one_lease_and_allows_another_database_inside_sql() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    one_reader(&store);
    let pool = store.reader_pool(Db::Library);
    let lease = pool.try_checkout().unwrap();
    store
        .with_reader_lease(Db::Library, lease, |store| {
            for _ in 0..2 {
                assert!(
                    store
                        .default_library(domain::MediaKind::Movie)
                        .unwrap()
                        .is_some()
                );
            }
            store
                .with_read(Db::Library, |_| {
                    store.list_users()?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(pool.available(), 0);
        })
        .unwrap();
    assert_eq!(pool.available(), 1);
}

#[test]
fn a_bound_reader_keeps_one_snapshot_across_store_methods() {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writer = Store::open(dir.path()).unwrap();
    one_reader(&store);
    let pool = store.reader_pool(Db::Library);
    let lease = pool.try_checkout().unwrap();
    let before = store
        .with_reader_lease(Db::Library, lease, |store| {
            let before = store.list_libraries().unwrap();
            writer
                .create_library(
                    domain::MediaKind::Movie,
                    "Committed during request",
                    &["/reader-snapshot/new"],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            let during = store.list_libraries().unwrap();
            assert_eq!(
                during, before,
                "one admitted reader must retain its snapshot between Store methods"
            );
            assert_eq!(pool.available(), 0);
            before
        })
        .unwrap();
    assert_eq!(pool.available(), 1);
    assert_eq!(store.list_libraries().unwrap().len(), before.len() + 1);
}

#[test]
fn a_scope_rejects_a_reader_from_another_database_or_store() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path().join("one")).unwrap();
    let other = Store::open(dir.path().join("two")).unwrap();
    for lease in [
        store.reader_pool(Db::App).try_checkout().unwrap(),
        other.reader_pool(Db::Library).try_checkout().unwrap(),
    ] {
        assert!(matches!(
            store.with_reader_lease(Db::Library, lease, |_| panic!("foreign reader admitted")),
            Err(StoreError::ReadLeaseMismatch)
        ));
    }
}

#[test]
fn same_database_reentry_fails_without_panicking_or_checking_out_again() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    one_reader(&store);
    let pool = store.reader_pool(Db::Library);
    let lease = pool.try_checkout().unwrap();
    let result = store
        .with_reader_lease(Db::Library, lease, |store| {
            store.with_read(Db::Library, |_| {
                store.default_library(domain::MediaKind::Movie)
            })
        })
        .unwrap();
    assert!(matches!(result, Err(StoreError::ReadReentrant)));
    assert_eq!(pool.available(), 1);
}

#[test]
fn unwinding_a_reader_scope_releases_the_connection_and_restores_normal_reads() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    one_reader(&store);
    let pool = store.reader_pool(Db::Library);
    let lease = pool.try_checkout().unwrap();
    let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = store.with_reader_lease(Db::Library, lease, |_| panic!("fixture panic"));
    }));
    assert!(panic.is_err());
    assert_eq!(pool.available(), 1);
    assert!(
        store
            .default_library(domain::MediaKind::Movie)
            .unwrap()
            .is_some()
    );
}

#[test]
fn ordinary_library_reads_keep_diagnostics_without_invoking_page_observers() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = store.reader_pool(Db::Library);
    let snapshots = Arc::new(AtomicUsize::new(0));
    let progress = Arc::new(AtomicUsize::new(0));
    let diagnostic = Arc::new(AtomicUsize::new(0));
    let calls = snapshots.clone();
    pool.set_snapshot_observer(Some(Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
    })));
    let calls = progress.clone();
    pool.set_query_progress_observer(Some(Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
    })));
    let calls = diagnostic.clone();
    store.with_library_progress(
        1,
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            false
        },
        || {
            assert!(
                store
                    .default_library(domain::MediaKind::Movie)
                    .unwrap()
                    .is_some()
            );
        },
    );
    assert_eq!(snapshots.load(Ordering::SeqCst), 0);
    assert_eq!(progress.load(Ordering::SeqCst), 0);
    assert!(diagnostic.load(Ordering::SeqCst) > 0);
}
