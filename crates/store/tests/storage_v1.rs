use domain::{Media, MediaId, MediaKind};
use rusqlite::Connection;
use store::{Store, StoreError};

use media::response_cache::{CacheSource, initialize as initialize_cache, put as put_cache};

fn cache_schema(conn: &Connection) -> (u32, i64, Vec<(String, String)>) {
    let app_id = conn
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .unwrap();
    let user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT type, name FROM sqlite_schema WHERE type IN ('table','view','index','trigger') AND name NOT LIKE 'sqlite_%' ORDER BY type, name")
        .unwrap();
    let objects = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (app_id, user_version, objects)
}

#[test]
fn store_and_media_cache_initializers_converge_in_both_orders() {
    let tmp = tempfile::tempdir().unwrap();
    let media_first_dir = tmp.path().join("media-first");
    std::fs::create_dir_all(&media_first_dir).unwrap();
    let media_first_path = media_first_dir.join("cache.db");
    let media_conn = Connection::open(&media_first_path).unwrap();
    initialize_cache(&media_conn).unwrap();
    put_cache(&media_conn, CacheSource::Tmdb, "/stored", "body", 123, None).unwrap();
    drop(media_conn);

    drop(Store::open(&media_first_dir).unwrap());
    let media_first_conn = Connection::open(&media_first_path).unwrap();
    let media_first_schema = cache_schema(&media_first_conn);
    let row = media::response_cache::get(&media_first_conn, CacheSource::Tmdb, "/stored")
        .unwrap()
        .unwrap();
    assert_eq!(row.body, "body");
    assert_eq!(row.fetched_at, 123);
    drop(media_first_conn);

    let store_first_dir = tmp.path().join("store-first");
    drop(Store::open(&store_first_dir).unwrap());
    let store_first_path = store_first_dir.join("cache.db");
    let store_conn = Connection::open(&store_first_path).unwrap();
    initialize_cache(&store_conn).unwrap();
    put_cache(&store_conn, CacheSource::Tmdb, "/stored", "body", 123, None).unwrap();
    let store_first_schema = cache_schema(&store_conn);
    let row = media::response_cache::get(&store_conn, CacheSource::Tmdb, "/stored")
        .unwrap()
        .unwrap();
    assert_eq!(row.body, "body");
    assert_eq!(row.fetched_at, 123);

    assert_eq!(media_first_schema, store_first_schema);
    assert_eq!(
        store_first_schema.0,
        media::response_cache::CACHE_APPLICATION_ID
    );
    assert_eq!(
        store_first_schema.1,
        media::response_cache::CACHE_USER_VERSION as i64
    );
    // One DDL owner: the media cache schema declares two indexes per source table.
    assert_eq!(
        store_first_schema
            .2
            .iter()
            .filter(|(kind, _)| kind == "index")
            .count(),
        12
    );
}

/// An incomplete current cache definition is detected without writes.
#[test]
fn incomplete_cache_definition_is_rejected_without_rebuild_action() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    {
        let store = Store::open(&data_dir).unwrap();
        store
            .put_catalog_cache("tmdb", "/movie/100", "cached_body", 100, Some(200))
            .unwrap();
    }
    let cache_path = data_dir.join("cache.db");
    Connection::open(&cache_path)
        .unwrap()
        .execute_batch("DROP INDEX idx_tmdb_cache_retention")
        .unwrap();
    let before = cache_schema(&Connection::open(&cache_path).unwrap());

    let error = match Store::open(&data_dir) {
        Ok(_) => panic!("incomplete cache definition must not start the store"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("idx_tmdb_cache_retention"),
        "error must name the missing declared index: {message}"
    );
    assert!(
        !message.contains("storage-cutover-rebuild.sh"),
        "startup must not propose a historical rebuild action: {message}"
    );

    let after = cache_schema(&Connection::open(&cache_path).unwrap());
    assert_eq!(
        before, after,
        "rejecting an incomplete cache must not mutate it"
    );
}

#[test]
fn missing_declared_table_or_index_in_facts_db_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    // Initialize clean store
    drop(Store::open(&data_dir).unwrap());

    // 1. Corrupt app.db by dropping an index
    let app_db_path = data_dir.join("app.db");
    {
        let conn = Connection::open(&app_db_path).unwrap();
        conn.execute("DROP INDEX sites_enabled_idx;", []).unwrap();
    }
    let err = Store::open(&data_dir).expect_err("app.db missing index must be rejected");
    assert!(
        err.to_string()
            .contains("app database schema is missing required index sites_enabled_idx"),
        "unexpected error message: {err}"
    );
}

#[test]
fn fresh_layout_reopens_without_old_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    {
        let store = Store::open(&data_dir).expect("Store::open should succeed on clean dir");
        assert!(data_dir.join("app.db").is_file());
        assert!(data_dir.join("cache.db").is_file());
        assert!(data_dir.join("library.db").is_file());
        assert!(data_dir.join("subscribe.db").is_file());

        assert!(
            !data_dir.join("catalog.db").exists(),
            "catalog.db must not be created"
        );
        assert!(
            !data_dir.join("fanart.db").exists(),
            "fanart.db must not be created"
        );

        assert_eq!(store.cache_db_path(), data_dir.join("cache.db"));

        // Check seeded libraries exist in library.db
        let libs = store.list_libraries().expect("list_libraries");
        assert!(
            libs.len() >= 2,
            "Seeded default movie and tv libraries must exist"
        );
    }

    // Reopen must succeed idempotently
    {
        let store = Store::open(&data_dir).expect("Store::open idempotent reopen");
        let libs = store.list_libraries().expect("list_libraries on reopen");
        assert!(libs.len() >= 2);
    }
}

#[test]
fn wrong_layout_is_rejected_without_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let app_file = data_dir.join("app.db");
    {
        let conn = Connection::open(&app_file).unwrap();
        // Set invalid application_id and user_version = 99
        conn.pragma_update(None, "application_id", 9999).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        conn.execute("CREATE TABLE sentinel (id INTEGER);", [])
            .unwrap();
    }

    let err = Store::open(&data_dir).expect_err("Store::open must reject invalid layout");
    match err {
        StoreError::Invalid(msg) => {
            assert!(
                msg.contains("app database layout mismatch"),
                "Expected layout mismatch message: {msg}"
            );
        }
        other => panic!("Expected StoreError::Invalid, got {other:?}"),
    }

    // File must not have been modified by Store::open
    let conn = Connection::open(&app_file).unwrap();
    let app_id: i64 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, 9999);
    assert_eq!(user_ver, 99);
}

#[test]
fn media_and_aliases_isolation_and_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let movie = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: vec!["The Matrix".into()],
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&movie).expect("insert movie");

    // TV show with same TMDB numeric id (603) must be isolated and allowed
    let tv = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Some TV".into(),
        year: Some(2005),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store
        .insert_media(&tv)
        .expect("insert tv with same numeric id as movie must succeed");

    // Another movie with same TMDB id (603) must fail (conflict)
    let duplicate_movie = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Matrix Duplicate".into(),
        year: Some(1999),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let err = store
        .insert_media(&duplicate_movie)
        .expect_err("duplicate movie alias must fail");
    assert!(matches!(
        err,
        StoreError::Protected(_) | StoreError::Sqlite(_)
    ));

    // Read back movie and tv by ID
    let fetched_movie = store
        .get_media(movie.id)
        .unwrap()
        .expect("movie should be found");
    assert_eq!(fetched_movie.title, "Matrix");
    assert_eq!(fetched_movie.tmdb_id.as_deref(), Some("603"));

    let fetched_tv = store.get_media(tv.id).unwrap().expect("tv should be found");
    assert_eq!(fetched_tv.title, "Some TV");
    assert_eq!(fetched_tv.tmdb_id.as_deref(), Some("603"));
}
