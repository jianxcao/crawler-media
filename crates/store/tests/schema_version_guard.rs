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
fn app_database_with_retired_tables_is_rejected_without_repair() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let path = tmp.path().join("app.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE naming_templates(kind TEXT PRIMARY KEY,pattern TEXT NOT NULL)")
        .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(
        Store::open(tmp.path()).is_err(),
        "retired table must not be accepted as the current layout"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
