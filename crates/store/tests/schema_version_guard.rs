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
fn subscribe_v2_fresh_open_and_reopen_are_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let store = Store::open(tmp.path()).unwrap();
        let actual = versions(&store);
        assert_eq!(actual["subscribe"], 2);
        assert_eq!(actual["app"], 1);
        assert_eq!(actual["cache"], 1);
        assert_eq!(actual["library"], 5);
    }
}

fn database_snapshot(dir: &Path) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
    use sha2::{Digest, Sha256};

    std::fs::read_dir(dir)
        .unwrap()
        .map(Result::unwrap)
        // SQLite can create WAL/SHM sidecars while reading a WAL-mode file.
        // The authoritative database bytes and set of database files must stay intact.
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "db"))
        .map(|entry| {
            let hash = Sha256::digest(std::fs::read(entry.path()).unwrap()).to_vec();
            (entry.file_name(), hash)
        })
        .collect()
}

#[test]
fn opening_subscribe_v1_is_rejected_before_any_file_changes() {
    for other_files_exist in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("subscribe.db");
        if other_files_exist {
            drop(Store::open(tmp.path()).unwrap());
            std::fs::remove_file(&path).unwrap();
        }
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(include_str!("fixtures/subscribe_v1.sql"))
                .unwrap();
            db.pragma_update(None, "application_id", 1129139250)
                .unwrap();
            db.pragma_update(None, "user_version", 1).unwrap();
            db.execute(
                "INSERT INTO subscribes
                 (id, user_id, media_id, coverage_kind, fetch_mode, filter_id)
                 VALUES ('untouched', 'user', 'media', 'movie', 'search', 'filter')",
                [],
            )
            .unwrap();
        }
        let before = database_snapshot(tmp.path());
        let error = Store::open(tmp.path()).expect_err("subscribe v1 must be refused");
        let store::StoreError::LayoutMismatch(message) = error else {
            panic!("expected a typed layout refusal, got {error:?}");
        };
        for required in ["subscribe", "user_version 2", "storage-cutover-rebuild.sh"] {
            assert!(message.contains(required), "missing {required}: {message}");
        }
        assert_eq!(
            database_snapshot(tmp.path()),
            before,
            "refusal must not create, migrate, reset or modify any database"
        );
    }
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

/// A file that carries something this build's layout does not declare is not a
/// file this build serves: it is refused, named, and left untouched.
///
/// One case instead of three per-database ones (`app_database_with_retired_tables…`,
/// `malformed_cache_layouts…`, `legacy_v7_database…`): the rule is the same for
/// every database, and the library file now enforces it too instead of only
/// checking that its *required* objects exist.
#[test]
fn unknown_object_is_rejected() {
    // library: a table the DDL does not declare. Every required object is still
    // present, so the required-object check alone would accept this file.
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let library = tmp.path().join("library.db");
    {
        let db = rusqlite::Connection::open(&library).unwrap();
        db.execute_batch("CREATE TABLE zz_extra(x)").unwrap();
    }
    let before = std::fs::read(&library).unwrap();
    match Store::open(tmp.path()) {
        Ok(_) => panic!("library.db with an undeclared table must not start the store"),
        Err(store::StoreError::LayoutMismatch(message)) => assert!(
            message.contains("zz_extra"),
            "the refusal must name the object it refused: {message}"
        ),
        Err(other) => panic!("expected StoreError::LayoutMismatch, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&library).unwrap(),
        before,
        "the refusal must not repair or reset the file"
    );

    // app: a retired Naming table, and a user_version this build does not serve.
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let app = tmp.path().join("app.db");
    {
        let db = rusqlite::Connection::open(&app).unwrap();
        db.execute_batch(
            "CREATE TABLE naming_templates(kind TEXT PRIMARY KEY,pattern TEXT NOT NULL)",
        )
        .unwrap();
    }
    let before = std::fs::read(&app).unwrap();
    assert!(
        Store::open(tmp.path()).is_err(),
        "a retired app table must not be accepted as the current layout"
    );
    assert_eq!(std::fs::read(&app).unwrap(), before);
    {
        let db = rusqlite::Connection::open(&app).unwrap();
        db.execute_batch("DROP TABLE naming_templates").unwrap();
    }
    set_user_version(&app, 7);
    match Store::open(tmp.path()) {
        Err(store::StoreError::Invalid(message)) => assert!(
            message.contains("app database layout mismatch"),
            "app refusals report the mismatch: {message}"
        ),
        other => {
            panic!("expected StoreError::Invalid for a foreign app user_version, got {other:?}")
        }
    }

    // cache: a derived file whose objects are not the declared ones. Two shapes:
    // a same-application-id file with a single-column table, and an index-only file.
    // The first shape is also what the cache DDL owner must refuse, so that seam is
    // asserted here instead of in a separate case.
    for (name, schema, cache_owner_refuses) in [
        (
            "malformed",
            "CREATE TABLE tmdb_cache (entry_id INTEGER PRIMARY KEY)",
            true,
        ),
        (
            "index-only",
            "CREATE TABLE unrelated (value INTEGER); \
             CREATE INDEX unrelated_idx ON unrelated(value);",
            false,
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cache.db");
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(schema).unwrap();
            db.pragma_update(
                None,
                "application_id",
                media::response_cache::CACHE_APPLICATION_ID,
            )
            .unwrap();
            db.pragma_update(
                None,
                "user_version",
                media::response_cache::CACHE_USER_VERSION as i64,
            )
            .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert!(
            Store::open(tmp.path()).is_err(),
            "the {name} cache layout must be refused"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the {name} cache refusal must not mutate the derived file"
        );
        if cache_owner_refuses {
            let read_only = rusqlite::Connection::open_with_flags(
                &path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            assert!(
                media::response_cache::initialize(&read_only).is_err(),
                "the cache DDL owner must refuse the {name} layout too"
            );
        }
    }
}

#[test]
fn opening_library_v1_is_rejected_before_other_files_change() {
    use sha2::{Digest, Sha256};

    let fresh = tempfile::tempdir().unwrap();
    drop(Store::open(fresh.path()).unwrap());
    let old_copy = tempfile::tempdir().unwrap();
    let names = ["app", "cache", "library", "subscribe"];
    for name in names {
        let file = format!("{name}.db");
        std::fs::copy(fresh.path().join(&file), old_copy.path().join(&file)).unwrap();
    }
    set_user_version(&old_copy.path().join("library.db"), 1);
    let hashes = || {
        names.map(|name| {
            Sha256::digest(std::fs::read(old_copy.path().join(format!("{name}.db"))).unwrap())
        })
    };
    let before = hashes();
    let error = Store::open(old_copy.path()).expect_err("old library layout must be refused");
    let message = error.to_string();
    for required in [
        "library",
        "user_version 5",
        "preserve",
        "CRAWLER_MEDIA_DATA",
        "docs/deploy.md",
    ] {
        assert!(message.contains(required), "missing {required}: {message}");
    }
    assert_eq!(hashes(), before);
}

/// The index arm of the layout check is strict in **both** directions.
///
/// Direction one was silently missing for part of the layout: `LIBRARY_INDEXES` omitted
/// `files_kind_path` and `terms_value`, so "every required index exists" never verified
/// them (the two lists are equal in length now — 73 — and the case below drops one of
/// those very names, which is the regression guard for the omission itself).
///
/// Direction two is new in v5: an *extra* index is not harmless drift on this branch. It
/// is write cost on every insert that no read path pays for, i.e. exactly what the v4/v5
/// rounds removed, and a file carrying one was not built by this DDL. So an extra index
/// is refused like an extra table — with the same typed error and the same runnable
/// rebuild hint, so the operator path is unchanged.
///
/// Triggers stay allowed on purpose: a fixture installs one to simulate a failure
/// mid-window (`tests/storage_work_bounds/scan_windows.rs`), and a trigger is not part of
/// the declared storage layout.
#[test]
fn index_layout_is_strict_in_both_directions() {
    for (case, statement, named) in [
        (
            "a required index is missing",
            "DROP INDEX terms_value",
            "terms_value",
        ),
        (
            "an index the layout does not declare is present",
            "CREATE INDEX zz_extra_idx ON library_items(library_id)",
            "zz_extra_idx",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        drop(Store::open(&dir).unwrap());
        let library = dir.join("library.db");
        {
            let db = rusqlite::Connection::open(&library).unwrap();
            db.execute_batch(statement)
                .unwrap_or_else(|error| panic!("{case}: {statement}: {error}"));
        }
        let before = std::fs::read(&library).unwrap();
        let error = match Store::open(&dir) {
            Ok(_) => panic!("{case}: the store must refuse this library.db"),
            Err(error) => error,
        };
        let message = error.to_string();
        for required in [
            named,
            "user_version 5",
            "scripts/storage-cutover-rebuild.sh",
        ] {
            assert!(
                message.contains(required),
                "{case}: missing {required} in: {message}"
            );
        }
        assert_eq!(
            std::fs::read(&library).unwrap(),
            before,
            "{case}: the refusal must not repair or reset the file"
        );
    }
}
