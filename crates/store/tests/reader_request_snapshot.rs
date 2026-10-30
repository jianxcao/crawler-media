//! Pure-read request stages retain one reader per database and one snapshot.
use std::{num::NonZeroUsize, time::Duration};
use store::{Db, Store};

fn fixture() -> tempfile::TempDir {
    let root = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn one_reader(store: &Store, db: Db) {
    store
        .configure_reader_pool_size(db, NonZeroUsize::new(1).unwrap())
        .unwrap();
}

#[test]
fn one_read_snapshot_checks_out_once_for_all_library_reads() {
    let dir = fixture();
    let store = Store::open(dir.path()).unwrap();
    one_reader(&store, Db::Library);
    let pool = store.reader_pool(Db::Library);
    let (result, stats) = store.with_stats(store::stats::StatsScope::Library, || {
        store.with_read_snapshot(Duration::from_secs(1), || {
            let library = store
                .default_library(domain::MediaKind::Movie)
                .unwrap()
                .unwrap();
            assert_eq!(pool.available(), 0, "scope retains its admitted reader");
            assert_eq!(store.get_library(&library.id).unwrap().unwrap(), library);
            assert!(
                store
                    .list_libraries()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.id == library.id)
            );
        })
    });
    result.unwrap();
    assert_eq!(
        stats.reader_checkouts, 1,
        "a request read scope must pay admission once, independent of Store read count"
    );
    assert_eq!(pool.available(), 1);
}

#[test]
fn cache_reads_reuse_the_request_reader_and_keep_their_snapshot() {
    let dir = fixture();
    let store = Store::open(dir.path()).unwrap();
    let writer = Store::open(dir.path()).unwrap();
    one_reader(&store, Db::Cache);
    store
        .put_catalog_cache("tmdb", "request-snapshot", "before", 1, None)
        .unwrap();
    let pool = store.reader_pool(Db::Cache);
    store
        .with_read_snapshot(Duration::from_secs(1), || {
            assert_eq!(
                store.get_catalog_cache("tmdb", "request-snapshot").unwrap(),
                Some("before".into())
            );
            assert_eq!(pool.available(), 0, "Cache reader remains bound");
            writer
                .put_catalog_cache("tmdb", "request-snapshot", "after", 2, None)
                .unwrap();
            assert_eq!(
                store.get_catalog_cache("tmdb", "request-snapshot").unwrap(),
                Some("before".into()),
                "Cache facts must come from the request's original snapshot"
            );
            let cached = store.list_catalog_cache().unwrap();
            assert_eq!(cached.len(), 1);
            assert_eq!(cached[0].body, "before");
        })
        .unwrap();
    assert_eq!(pool.available(), 1);
    assert_eq!(
        store.get_catalog_cache("tmdb", "request-snapshot").unwrap(),
        Some("after".into()),
        "the next request must see the writer's committed change"
    );
}

#[test]
fn nested_request_scopes_reuse_both_databases_and_preserve_each_snapshot() {
    let dir = fixture();
    let store = Store::open(dir.path()).unwrap();
    let writer = Store::open(dir.path()).unwrap();
    for db in [Db::Library, Db::Cache] {
        one_reader(&store, db);
    }
    store
        .put_catalog_cache("tmdb", "nested", "before", 1, None)
        .unwrap();
    let (result, stats) = store.with_stats(store::stats::StatsScope::Library, || {
        store.with_read_snapshot(Duration::from_secs(1), || {
            let libraries = store.list_libraries().unwrap();
            assert_eq!(
                store.get_catalog_cache("tmdb", "nested").unwrap(),
                Some("before".into())
            );
            writer
                .create_library(
                    domain::MediaKind::Movie,
                    "New library",
                    &["/nested/new"],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            writer
                .put_catalog_cache("tmdb", "nested", "after", 2, None)
                .unwrap();
            store
                .with_read_snapshot(Duration::from_secs(1), || {
                    assert_eq!(store.list_libraries().unwrap(), libraries);
                    assert_eq!(
                        store.get_catalog_cache("tmdb", "nested").unwrap(),
                        Some("before".into())
                    );
                    for db in [Db::Library, Db::Cache] {
                        assert_eq!(store.reader_pool(db).available(), 0);
                    }
                })
                .unwrap();
            assert_eq!(store.list_libraries().unwrap(), libraries);
            assert_eq!(
                store.get_catalog_cache("tmdb", "nested").unwrap(),
                Some("before".into())
            );
            for db in [Db::Library, Db::Cache] {
                assert_eq!(
                    store.reader_pool(db).available(),
                    0,
                    "inner scope must leave outer readers bound"
                );
            }
            libraries.len()
        })
    });
    let old_count = result.unwrap();
    assert_eq!(
        stats.reader_checkouts, 1,
        "nested scopes reuse the outer Library reader"
    );
    for db in [Db::Library, Db::Cache] {
        assert_eq!(store.reader_pool(db).available(), 1);
    }
    assert_eq!(store.list_libraries().unwrap().len(), old_count + 1);
    assert_eq!(
        store.get_catalog_cache("tmdb", "nested").unwrap(),
        Some("after".into())
    );
}
