use std::path::Path;
use rusqlite::Connection;
use store::schema::layout::{
    APP_APPLICATION_ID, CACHE_APPLICATION_ID, LIBRARY_APPLICATION_ID, SUBSCRIBE_APPLICATION_ID,
    V1_USER_VERSION,
};
use store::{Store, StoreError};
use domain::{Confidence, Media, MediaId, MediaKind, QualitySource};

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

        assert!(!data_dir.join("catalog.db").exists(), "catalog.db must not be created");
        assert!(!data_dir.join("fanart.db").exists(), "fanart.db must not be created");

        assert_eq!(store.cache_db_path(), data_dir.join("cache.db"));

        // Check seeded libraries exist in library.db
        let libs = store.list_libraries().expect("list_libraries");
        assert!(libs.len() >= 2, "Seeded default movie and tv libraries must exist");
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
        conn.execute("CREATE TABLE sentinel (id INTEGER);", []).unwrap();
    }

    let err = Store::open(&data_dir).expect_err("Store::open must reject invalid layout");
    match err {
        StoreError::Invalid(msg) => {
            assert!(msg.contains("app database layout mismatch"), "Expected layout mismatch message: {msg}");
        }
        other => panic!("Expected StoreError::Invalid, got {other:?}"),
    }

    // File must not have been modified by Store::open
    let conn = Connection::open(&app_file).unwrap();
    let app_id: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0)).unwrap();
    let user_ver: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
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
    store.insert_media(&tv).expect("insert tv with same numeric id as movie must succeed");

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
    let err = store.insert_media(&duplicate_movie).expect_err("duplicate movie alias must fail");
    assert!(matches!(err, StoreError::Protected(_) | StoreError::Sqlite(_)));

    // Read back movie and tv by ID
    let fetched_movie = store.get_media(movie.id).unwrap().expect("movie should be found");
    assert_eq!(fetched_movie.title, "Matrix");
    assert_eq!(fetched_movie.tmdb_id.as_deref(), Some("603"));

    let fetched_tv = store.get_media(tv.id).unwrap().expect("tv should be found");
    assert_eq!(fetched_tv.title, "Some TV");
    assert_eq!(fetched_tv.tmdb_id.as_deref(), Some("603"));
}
