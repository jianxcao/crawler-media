use parking_lot::Mutex;
use std::sync::Arc;
use store::Store;

#[path = "management/common.rs"]
mod common;

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

#[test]
fn jobs_db_layout_gate_rejects_missing_tables_with_backup_guidance() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Create an invalid jobs.db (missing tables or wrong application_id)
    let jobs_db_path = data_dir.join("jobs.db");
    {
        let conn = rusqlite::Connection::open(&jobs_db_path).unwrap();
        conn.execute("CREATE TABLE wrong_table (id INTEGER PRIMARY KEY);", [])
            .unwrap();
        conn.pragma_update(None, "application_id", 999999).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }

    // Creating ApiState must be rejected
    let fetcher = std::sync::Arc::new(common::Fixtures {
        requests: parking_lot::Mutex::new(Vec::new()),
        bodies: std::collections::HashMap::new(),
    });
    let downloader =
        std::sync::Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage")));
    let res = api::ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp.path().join("library"),
    );

    let err = match res {
        Ok(_) => panic!("mismatched jobs.db must be rejected"),
        Err(e) => e,
    };
    let err_str = err.to_string();
    assert!(err_str.contains("jobs layout mismatch"));
    assert!(err_str.contains("jobs.db contains durable Job definitions and states"));
    assert!(err_str.contains("Do not delete or recreate it automatically"));
    assert!(err_str.contains("back up data/jobs.db"));
}

/// Copy a current instance into a temporary directory and validate its complete layout.
/// Populated unstamped queues are refused; this fixture performs no automatic adoption.
///
/// 路径用 `CARGO_MANIFEST_DIR` 解析（cargo 运行测试时的工作目录是包根，不是仓库根）。
/// 需要真实实例数据，默认 ignore（不再在缺数据时静默算作通过）：
/// `LIVE_DATA_DIR=/path/to/data/live cargo test -p api --test storage_cache_runtime -- --ignored`，
/// 未设置时使用仓库内 `data/live`；目录缺失即失败。
#[test]
#[ignore = "needs a copy of a live instance's data/live (set LIVE_DATA_DIR); run with --ignored"]
fn live_databases_copy_is_accepted_and_stamped_properly() {
    let live_dir = std::env::var_os("LIVE_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/live")
        });
    assert!(
        live_dir.is_dir(),
        "{} is absent: set LIVE_DATA_DIR to a live data directory",
        live_dir.display()
    );

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let db_names = [
        "app.db",
        "library.db",
        "subscribe.db",
        "cache.db",
        "jobs.db",
    ];
    for db_name in db_names {
        let src = live_dir.join(db_name);
        assert!(
            src.is_file(),
            "expected the existing instance to have {}",
            src.display()
        );
        let dst = data_dir.join(db_name);
        std::fs::copy(&src, &dst).unwrap();
        assert_eq!(
            std::fs::read(&src).unwrap(),
            std::fs::read(&dst).unwrap(),
            "copy of {db_name} must be byte-identical"
        );
    }

    let fetcher = std::sync::Arc::new(common::Fixtures {
        requests: parking_lot::Mutex::new(Vec::new()),
        bodies: std::collections::HashMap::new(),
    });
    let downloader =
        std::sync::Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage")));
    let _state = api::ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp.path().join("library"),
    )
    .expect("live databases copy must be accepted and successfully initialized");

    // A copied current queue must retain the identity owned by the Jobs crate.
    for db_name in db_names {
        let conn = rusqlite::Connection::open_with_flags(
            live_dir.join(db_name),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        if db_name == "jobs.db" {
            jobs::schema::validate_layout(&conn).unwrap();
        }
    }

    let jobs_conn = rusqlite::Connection::open(data_dir.join("jobs.db")).unwrap();
    let app_id: u32 = jobs_conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = jobs_conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, jobs::schema::JOBS_APPLICATION_ID);
    assert_eq!(user_ver, jobs::schema::JOBS_USER_VERSION);
}

#[test]
fn unstamped_jobs_db_is_rejected_without_changing_objects_or_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let jobs_path = data_dir.join("jobs.db");

    // 1. Build standard jobs schema using Queue::open
    {
        drop(jobs::Queue::open(&jobs_path).unwrap());
        let conn = rusqlite::Connection::open(&jobs_path).unwrap();
        // Clear application_id and user_version to 0/0 to simulate unstamped existing DB
        conn.pragma_update(None, "application_id", 0).unwrap();
        conn.pragma_update(None, "user_version", 0).unwrap();
    }

    let before_schema: Vec<String> = {
        let conn = rusqlite::Connection::open(&jobs_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT type||':'||name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let fetcher = std::sync::Arc::new(common::Fixtures {
        requests: parking_lot::Mutex::new(Vec::new()),
        bodies: std::collections::HashMap::new(),
    });
    let downloader =
        std::sync::Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage")));

    // Unsupported populated files are rejected before any automatic adoption.
    let result = api::ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp.path().join("library"),
    );
    assert!(result.is_err());

    // Rejection preserves the unsupported identity headers and schema.
    let conn = rusqlite::Connection::open(&jobs_path).unwrap();
    let app_id: u32 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!((app_id, user_ver), (0, 0));

    let after_schema: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT type||':'||name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(before_schema, after_schema);
}

#[test]
fn stamped_jobs_db_missing_declared_index_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let jobs_path = data_dir.join("jobs.db");

    // A current identity still requires every declared schema object.
    {
        drop(jobs::Queue::open(&jobs_path).unwrap());
        let conn = rusqlite::Connection::open(&jobs_path).unwrap();
        conn.pragma_update(None, "application_id", jobs::schema::JOBS_APPLICATION_ID)
            .unwrap();
        conn.pragma_update(None, "user_version", jobs::schema::JOBS_USER_VERSION)
            .unwrap();
        conn.execute("DROP INDEX jobs_last_def_idx;", []).unwrap();
    }

    let fetcher = std::sync::Arc::new(common::Fixtures {
        requests: parking_lot::Mutex::new(Vec::new()),
        bodies: std::collections::HashMap::new(),
    });
    let downloader =
        std::sync::Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage")));

    // 2. ApiState::new must fail
    let res = api::ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp.path().join("library"),
    );

    let err = match res {
        Ok(_) => panic!("jobs.db missing a declared index must be rejected"),
        Err(e) => e,
    };
    let err_str = err.to_string();
    assert!(
        err_str.contains("jobs_last_def_idx"),
        "error message must name the missing index: {err_str}"
    );
    assert!(
        err_str.contains("back up data/jobs.db"),
        "error message must contain backup hint: {err_str}"
    );

    // Rejection must preserve the current identity headers.
    let conn = rusqlite::Connection::open(&jobs_path).unwrap();
    let app_id: u32 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, jobs::schema::JOBS_APPLICATION_ID);
    assert_eq!(user_ver, jobs::schema::JOBS_USER_VERSION);
}
