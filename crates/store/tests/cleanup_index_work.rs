//! Q1: reverse-reference cleanup paths must not full-scan the child table.
//! Budget: the plan for each real cleanup statement and each FK parent-key
//! check searches by the reverse key instead of scanning unrelated rows.

use rusqlite::Connection;
use store::Store;

fn open_library_db() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    drop(Store::open(&data).unwrap());
    let conn = Connection::open(data.join("library.db")).unwrap();
    (tmp, conn)
}

fn plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let n = stmt.parameter_count();
    let args: Vec<String> = (0..n).map(|_| "x".to_string()).collect();
    stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
        r.get::<_, String>(3)
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn full_scans(conn: &Connection, sql: &str, table: &str) -> Vec<String> {
    plan(conn, sql)
        .into_iter()
        .filter(|d| d.starts_with(&format!("SCAN {table}")) && !d.contains("COVERING INDEX"))
        .collect()
}

/// Statements issued by real cleanup paths (data_retention prune, root removal).
const CLEANUP: &[(&str, &str)] = &[
    (
        "probe_stage_state",
        "SELECT 1 FROM probe_stage_state WHERE active_job_id = ?1",
    ),
    (
        "unidentified",
        "SELECT 1 FROM unidentified WHERE root_id = ?1",
    ),
];

#[test]
fn cleanup_statements_search_by_reverse_key() {
    let (_tmp, conn) = open_library_db();
    let mut failures = Vec::new();
    for (table, sql) in CLEANUP {
        let scans = full_scans(&conn, sql, table);
        println!("baseline {sql}: {:?}", plan(&conn, sql));
        if !scans.is_empty() {
            failures.push(format!("{sql}: {scans:?}"));
        }
    }
    assert!(failures.is_empty(), "full scans: {failures:#?}");
}

/// Deleting or re-keying a parent row makes SQLite look up children by the FK
/// columns; without a prefix index that is a scan of the whole child table.
#[test]
fn foreign_key_children_are_prefix_indexed() {
    let (_tmp, conn) = open_library_db();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut missing = Vec::new();
    for table in &tables {
        let mut fks: std::collections::BTreeMap<i64, Vec<String>> = Default::default();
        let mut stmt = conn
            .prepare(&format!("PRAGMA foreign_key_list('{table}')"))
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(3)?)))
            .unwrap();
        for row in rows {
            let (id, col) = row.unwrap();
            fks.entry(id).or_default().push(col);
        }
        for cols in fks.values() {
            let pred = cols
                .iter()
                .map(|c| format!("{c} = ?"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let sql = format!("SELECT 1 FROM {table} WHERE {pred}");
            let scans = full_scans(&conn, &sql, table);
            if !scans.is_empty() {
                missing.push(format!("{table}({})", cols.join(",")));
            }
        }
    }
    println!("baseline unindexed FK children: {missing:#?}");
    assert!(missing.is_empty(), "unindexed FK children: {missing:#?}");
}
