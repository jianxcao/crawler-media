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
    assert!(err_str.contains("jobs database layout mismatch"));
    assert!(err_str.contains("jobs.db is an active queue database"));
    assert!(err_str.contains("Do not delete or recreate automatically"));
    assert!(err_str.contains("back up data/jobs.db"));
}

/// 干净切换的验收面：既有实例（`data/live` 的五个库）必须能被启动校验接受，且既有
/// 未打标的 `jobs.db` 只被补写身份头。测试只读复制到临时目录，不碰原库。
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

    // 原库 jobs.db 只允许两种合法态：`(0,0)`（未切换的检出），或被启动闸门压印成
    // `(JOBS_APPLICATION_ID, JOBS_USER_VERSION)`（本机真实实例已被一次真实启动消耗）。
    // 依据：计划 `docs/superpowers/plans/2026-10-20-storage-clean-cutover.md:600` 已把
    // 「`data/live/jobs.db` 无身份闸门」记为**已加**——`crates/api/src/management/state.rs:233`
    // 在 `jobs::Queue::open` 之前调用 `store::schema::layout::initialize_jobs_layout`，
    // 压印点是 `crates/store/src/schema/layout.rs:88-89`。其它任何取值都意味着外来写入或串库，
    // 必须 panic；「未打标 → 补写身份头」路径本身由本文件下方 `unstamped_jobs_db_*` 两个用例覆盖。
    let unstamped = (0, 0);
    let stamped = (
        store::schema::layout::JOBS_APPLICATION_ID,
        store::schema::layout::JOBS_USER_VERSION,
    );
    for db_name in db_names {
        let conn = rusqlite::Connection::open_with_flags(
            live_dir.join(db_name),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let app_id: u32 = conn
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap();
        let user_ver: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        eprintln!("live/{db_name}: application_id={app_id} user_version={user_ver}");
        if db_name == "jobs.db" {
            assert!(
                (app_id, user_ver) == unstamped || (app_id, user_ver) == stamped,
                "live/jobs.db must be unstamped {unstamped:?} or startup-stamped {stamped:?}, \
                 got ({app_id}, {user_ver})"
            );
        }
    }

    let jobs_conn = rusqlite::Connection::open(data_dir.join("jobs.db")).unwrap();
    let app_id: u32 = jobs_conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = jobs_conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, store::schema::layout::JOBS_APPLICATION_ID);
    assert_eq!(user_ver, store::schema::layout::JOBS_USER_VERSION);
}

#[test]
fn unstamped_jobs_db_with_complete_layout_is_adopted_without_dropping_objects() {
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

    // 2. ApiState::new must succeed on unstamped but complete jobs.db
    let _state = api::ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp.path().join("library"),
    )
    .expect("ApiState::new should adopt unstamped jobs.db");

    // 3. Verify pragma is updated and objects are identical
    let conn = rusqlite::Connection::open(&jobs_path).unwrap();
    let app_id: u32 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, store::schema::layout::JOBS_APPLICATION_ID);
    assert_eq!(user_ver, store::schema::layout::JOBS_USER_VERSION);

    let after_schema: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT type||':'||name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        before_schema, after_schema,
        "adoption must only stamp pragmas without altering schema objects"
    );
}

#[test]
fn unstamped_jobs_db_missing_declared_index_is_rejected_instead_of_adopted() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let jobs_path = data_dir.join("jobs.db");

    // 1. Build jobs schema, clear pragmas, drop declared index
    {
        drop(jobs::Queue::open(&jobs_path).unwrap());
        let conn = rusqlite::Connection::open(&jobs_path).unwrap();
        conn.pragma_update(None, "application_id", 0).unwrap();
        conn.pragma_update(None, "user_version", 0).unwrap();
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
        Ok(_) => panic!("unstamped jobs.db missing declared index must be rejected"),
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

    // 3. Rejected DB must NOT be stamped
    let conn = rusqlite::Connection::open(&jobs_path).unwrap();
    let app_id: u32 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    let user_ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, 0);
    assert_eq!(user_ver, 0);
}
