//! DB3：比程序更新的数据库必须拒绝降级打开，而且预检不得改动任何库。

use std::collections::HashMap;
use std::path::Path;

use store::Store;

fn user_version(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn set_user_version(path: &Path, version: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
}

fn versions(store: &Store) -> HashMap<&'static str, i64> {
    store.schema_versions().into_iter().collect()
}

#[test]
fn newer_database_is_rejected_without_touching_any_database() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let store = Store::open(&dir).unwrap();
    let before = versions(&store);
    store.put_setting("guard.sentinel", "keep").unwrap();
    drop(store);

    // cache 比程序新（必须被拒）；预检不得改动任何库。
    set_user_version(&dir.join("cache.db"), before["cache"] + 1);

    let error = match Store::open(&dir) {
        Ok(_) => panic!("newer schema must be rejected"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("cache"),
        "error should name the newer database: {message}"
    );
    assert!(
        message.contains(&(before["cache"] + 1).to_string()),
        "error should report the found version: {message}"
    );

    assert_eq!(user_version(&dir.join("app.db")), before["app"]);
    assert_eq!(user_version(&dir.join("cache.db")), before["cache"] + 1);
    assert_eq!(user_version(&dir.join("library.db")), before["library"]);
    assert_eq!(user_version(&dir.join("subscribe.db")), before["subscribe"]);

    let conn = rusqlite::Connection::open(dir.join("app.db")).unwrap();
    let sentinel: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'guard.sentinel'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sentinel, "keep", "existing data must be untouched");
}

#[test]
fn fresh_open_and_reopen_are_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let store = Store::open(&dir).unwrap();
    let expected = versions(&store);
    drop(store);

    // 幂等重开：版本不变。
    let store = Store::open(&dir).unwrap();
    assert_eq!(versions(&store), expected);
    drop(store);
}

#[test]
fn each_newer_database_is_rejected_individually() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let store = Store::open(&dir).unwrap();
    let before = versions(&store);
    drop(store);

    for name in ["app", "cache", "library", "subscribe"] {
        let path = dir.join(format!("{name}.db"));
        let original = user_version(&path);
        set_user_version(&path, before[name] + 1);
        let error = match Store::open(&dir) {
            Ok(_) => panic!("newer schema must be rejected"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(name),
            "error for {name} should name it: {error}"
        );
        assert_eq!(user_version(&path), before[name] + 1);
        set_user_version(&path, original);
    }
}

#[test]
fn unstamped_populated_jobs_layout_is_rejected_without_adopting_it() {
    use store::schema::{layout, v1};

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for table in v1::JOBS_TABLES {
        conn.execute_batch(&format!("CREATE TABLE {table} (id TEXT);"))
            .unwrap();
    }
    for index in v1::JOBS_INDEXES {
        conn.execute_batch(&format!("CREATE INDEX {index} ON jobs(id);"))
            .unwrap();
    }
    conn.execute("INSERT INTO jobs(id) VALUES ('preserve')", [])
        .unwrap();

    let error = layout::initialize_jobs_layout(&conn).unwrap_err();
    assert!(error.to_string().contains("layout mismatch"), "{error}");
    let application_id: i64 = conn
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!((application_id, version), (0, 0));
    let preserved: String = conn
        .query_row("SELECT id FROM jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(preserved, "preserve");
}
