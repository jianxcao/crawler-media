use rusqlite::Connection;
use sha2::{Digest, Sha256};
use store::Store;

fn hashes(dir: &std::path::Path) -> Vec<Vec<u8>> {
    ["app.db", "library.db", "subscribe.db", "cache.db"]
        .into_iter()
        .map(|file| Sha256::digest(std::fs::read(dir.join(file)).unwrap()).to_vec())
        .collect()
}

#[test]
fn previous_library_analysis_layout_is_refused_without_touching_the_database() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("library.db")).unwrap();
    conn.pragma_update(None, "user_version", 7).unwrap();
    drop(conn);
    let before = hashes(dir.path());
    let error = match Store::open(dir.path()) {
        Ok(_) => panic!("old Library layout is refused"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("user_version 8"));
    assert_eq!(hashes(dir.path()), before);
}

#[test]
fn matching_names_and_headers_do_not_accept_changed_table_or_index_definitions() {
    for database in ["app", "library", "subscribe", "cache"] {
        for change_index in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            drop(Store::open(dir.path()).unwrap());
            let conn = Connection::open(dir.path().join(format!("{database}.db"))).unwrap();
            let kind = if change_index { "index" } else { "table" };
            let (name,sql):(String,String)=conn.query_row(
                "SELECT name,sql FROM sqlite_schema WHERE type=?1 AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT 1",
                [kind],|r|Ok((r.get(0)?,r.get(1)?)),
            ).unwrap();
            let changed = if change_index {
                // Keep the required object name but replace the indexed expression.
                let table: String = conn
                    .query_row(
                        "SELECT tbl_name FROM sqlite_schema WHERE name=?1",
                        [&name],
                        |r| r.get(0),
                    )
                    .unwrap();
                let column: String = conn
                    .query_row(
                        &format!(
                            "SELECT name FROM pragma_table_info('{table}') ORDER BY cid LIMIT 1"
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                format!("CREATE INDEX {name} ON {table} ({column}) WHERE 0")
            } else {
                sql.replacen("TEXT", "BLOB", 1)
            };
            assert_ne!(sql, changed);
            conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
            conn.execute(
                "UPDATE sqlite_schema SET sql=?1 WHERE type=?2 AND name=?3",
                rusqlite::params![changed, kind, name],
            )
            .unwrap();
            conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
            drop(conn);
            let before = hashes(dir.path());
            assert!(
                Store::open(dir.path()).is_err(),
                "{database}: changed {kind} must not be admitted as the current schema"
            );
            assert_eq!(
                hashes(dir.path()),
                before,
                "schema validation must not repair or adopt a different definition"
            );
        }
    }
}

#[test]
fn prior_sites_layout_is_rejected_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='sites'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let old = sql.replace(
        ",revision INTEGER NOT NULL CHECK(revision > 0),credential_epoch TEXT NOT NULL",
        "",
    );
    assert_ne!(old, sql);
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    conn.execute("UPDATE sqlite_schema SET sql=?1 WHERE name='sites'", [&old])
        .unwrap();
    conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
    drop(conn);
    let before = hashes(dir.path());
    assert!(Store::open(dir.path()).is_err());
    assert_eq!(hashes(dir.path()), before);
}

#[test]
fn pre_plugin_and_extra_plugin_layouts_are_refused_without_adoption() {
    for old in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        if old {
            let names=conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND (name='plugins' OR name LIKE 'plugin_%')").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
            for name in names {
                conn.execute_batch(&format!("DROP TABLE {name}")).unwrap();
            }
        } else {
            conn.execute_batch("CREATE TABLE plugin_extra(value BLOB)")
                .unwrap();
        }
        drop(conn);
        let before = hashes(dir.path());
        assert!(Store::open(dir.path()).is_err());
        assert_eq!(hashes(dir.path()), before);
    }
}
