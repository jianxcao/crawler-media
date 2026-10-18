use std::sync::Arc;
use parking_lot::Mutex;
use store::Store;

#[test]
fn six_sources_runtime_isolation_and_exact_prefix_query() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).expect("open store");

    // Seed 6 sources with identical cache_key
    let key = "/movie/100";
    for source in ["tmdb", "douban", "tvdb", "bangumi", "anilist", "fanart"] {
        store
            .put_catalog_cache(
                source,
                key,
                &format!("payload_for_{source}"),
                1000,
                Some(2000),
            )
            .unwrap();
    }

    // Verify isolation in get_catalog_cache
    for source in ["tmdb", "douban", "tvdb", "bangumi", "anilist", "fanart"] {
        let cached = store.get_catalog_cache(source, key).unwrap().unwrap();
        assert_eq!(cached, format!("payload_for_{source}"));
    }

    // Deleting tmdb does not affect douban or others
    store.delete_catalog_cache("tmdb", key).unwrap();
    assert_eq!(store.get_catalog_cache("tmdb", key).unwrap(), None);
    assert_eq!(
        store.get_catalog_cache("douban", key).unwrap(),
        Some("payload_for_douban".into())
    );

    // List shows remaining 5 entries
    let list = store.list_catalog_cache().unwrap();
    assert_eq!(list.len(), 5);
}

#[test]
fn invalid_source_name_rejected_without_sql_injection() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).expect("open store");

    let res = store.put_catalog_cache(
        "invalid_source; DROP TABLE tmdb_cache;",
        "/key",
        "{}",
        1000,
        None,
    );
    assert!(res.is_err());
}
