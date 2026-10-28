//! The query read model is created only for a fresh Library database.

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::path::Path;
use store::Store;

const QUERY_TABLES: &[&str] = &[
    "library_protocol_terms",
    "library_protocol_people",
    "library_query_counts",
    "library_query_config",
    "library_query_count_rebuild",
];
const QUERY_INDEXES: &[&str] = &[
    "items_protocol_name_asc",
    "items_protocol_name_desc",
    "items_protocol_default",
    "items_protocol_default_explicit",
    "units_protocol_order",
    // `protocol_terms_value` and `protocol_people_name` were dropped by the
    // 2026-10-26 schema audit: nothing seeks them. See
    // docs/audits/2026-10-26-library-schema-audit.md and the DDL comments.
    "protocol_terms_media",
    "protocol_people_wire",
    "protocol_people_tmdb",
];

fn hashes(dir: &Path) -> Vec<Vec<u8>> {
    ["app", "cache", "library", "subscribe"]
        .map(|name| Sha256::digest(std::fs::read(dir.join(format!("{name}.db"))).unwrap()).to_vec())
        .into()
}

fn seed_card(db: &Connection) {
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
         INSERT INTO libraries(library_id,kind,name) VALUES ('query-library','movie','Query Library');
         INSERT INTO media(media_id,kind,title,identity_state)
         VALUES ('query-media','movie','Älpha','confirmed');
         INSERT INTO library_items(library_id,media_id,kind,title,title_sort,identity_state,
             metadata_revision,item_revision,added_at_sort,stable_shuffle_key,file_count,
             available_file_count,unknown_file_count,protocol_title_key,
             query_projection_revision,query_config_revision)
         VALUES ('query-library','query-media','movie','Älpha','Älpha','confirmed',
             3,7,42,'stable',1,0,1,'älpha',7,9);
         INSERT INTO library_operations(operation_id,library_id,kind,state,expected_revision,
             plan_json,created_at_ms,updated_at_ms)
         VALUES ('query-operation','query-library','integrity','checking',0,'{}',0,0);",
    )
    .unwrap();
}

fn seed_query_rows(db: &Connection) {
    seed_card(db);
    for kind in ["genre", "studio"] {
        db.execute(
            "INSERT INTO library_protocol_terms VALUES ('query-library','query-media',?1,'name:drama')",
            [kind],
        )
        .unwrap();
    }
    for role in ["Actor", "Director", "Writer"] {
        db.execute(
            "INSERT INTO library_protocol_people VALUES ('query-library','query-media',?1,'person','','wire')",
            [role],
        )
        .unwrap();
    }
    for kind in [
        "decade",
        "runtime",
        "language",
        "rating",
        "hdr",
        "stock",
        "genre",
        "country",
        "resolution",
        "collection",
    ] {
        db.execute(
            "INSERT INTO library_item_terms VALUES ('query-library',?1,'bucket','query-media')",
            [kind],
        )
        .unwrap();
    }
    for surface in ["browse", "protocol_work"] {
        db.execute(
            "INSERT INTO library_query_counts VALUES ('query-library',?1,'all','*',1)",
            [surface],
        )
        .unwrap();
        db.execute(
            "INSERT INTO library_query_count_rebuild VALUES ('query-operation',?1,'all','*',1)",
            [surface],
        )
        .unwrap();
    }
    db.execute(
        "UPDATE library_query_config SET default_locale='en-US',revision=9 WHERE id=1",
        [],
    )
    .unwrap();
}

fn query_rows(db: &Connection) -> Vec<Vec<String>> {
    let tables = QUERY_TABLES
        .iter()
        .copied()
        .chain(["library_items", "library_item_terms"]);
    tables
        .map(|table| {
            let mut stmt = db
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
                .unwrap();
            let columns = stmt.column_count();
            stmt.query_map([], |row| {
                Ok((0..columns)
                    .map(|column| format!("{:?}", row.get_ref(column).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        })
        .collect()
}

#[test]
fn query_layout_fresh_open_and_reopen_preserve_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let versions = store.schema_versions();
    assert_eq!(
        versions.iter().find(|v| v.0 == "library").unwrap().1,
        store::schema::layout::LIBRARY_USER_VERSION
    );
    assert_eq!(
        versions.iter().find(|v| v.0 == "subscribe").unwrap().1,
        store::schema::layout::SUBSCRIBE_USER_VERSION
    );
    assert!(
        versions
            .iter()
            .filter(|v| v.0 != "library" && v.0 != "subscribe")
            .all(|v| v.1 == 1)
    );
    drop(store);
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    let initial: (i64, Option<String>, i64) = db
        .query_row(
            "SELECT id,default_locale,revision FROM library_query_config",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(initial, (1, None, 0));
    seed_query_rows(&db);
    let before = query_rows(&db);
    drop(db);
    drop(Store::open(dir.path()).unwrap());
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    assert_eq!(query_rows(&db), before);
}

#[test]
fn empty_files_initialize_one_query_config_row() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["app", "cache", "library", "subscribe"] {
        std::fs::write(dir.path().join(format!("{name}.db")), []).unwrap();
    }
    for _ in 0..2 {
        drop(Store::open(dir.path()).unwrap());
        let db = Connection::open(dir.path().join("library.db")).unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM library_query_config", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn missing_query_objects_are_rejected_without_database_writes() {
    for (kind, names) in [("INDEX", QUERY_INDEXES), ("TABLE", QUERY_TABLES)] {
        for name in names {
            let dir = tempfile::tempdir().unwrap();
            drop(Store::open(dir.path()).unwrap());
            let db = Connection::open(dir.path().join("library.db")).unwrap();
            db.execute_batch(&format!("DROP {kind} {name}")).unwrap();
            drop(db);
            let before = hashes(dir.path());
            let error = Store::open(dir.path()).expect_err("incomplete layout must be refused");
            assert!(error.to_string().contains(name), "{error}");
            assert_eq!(hashes(dir.path()), before, "{name}");
        }
    }
}

#[test]
fn query_members_and_counts_enforce_constraints_and_cascade() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    seed_query_rows(&db);
    for sql in [
        "INSERT INTO library_item_terms VALUES ('query-library','overview','retired','query-media')",
        "INSERT INTO library_item_terms VALUES ('query-library','original_title','retired','query-media')",
        "INSERT INTO library_item_terms VALUES ('query-library','jellyfin_genre','retired','query-media')",
        "INSERT INTO library_item_terms VALUES ('query-library','person_id','retired','query-media')",
        "INSERT INTO library_item_terms VALUES ('query-library','person_tmdb_id','retired','query-media')",
        "INSERT INTO library_protocol_terms VALUES ('query-library','query-media','country','US')",
        "INSERT INTO library_protocol_terms VALUES ('query-library','missing','genre','drama')",
        "INSERT INTO library_protocol_people VALUES ('query-library','query-media','Producer','person','','wire')",
        "INSERT INTO library_protocol_people VALUES ('query-library','query-media','Actor','other',NULL,'wire')",
        "INSERT INTO library_query_counts VALUES ('query-library','unknown','all','*',1)",
        "INSERT INTO library_query_counts VALUES ('query-library','browse','genre','negative',-1)",
        "INSERT INTO library_query_counts VALUES ('missing','browse','all','*',1)",
        "INSERT INTO library_query_count_rebuild VALUES ('query-operation','browse','genre','negative',-1)",
        "INSERT INTO library_query_count_rebuild VALUES ('missing','browse','all','*',1)",
        "INSERT INTO library_query_config(id) VALUES (2)",
    ] {
        assert!(db.execute(sql, []).is_err(), "{sql}");
    }
    db.execute(
        "DELETE FROM library_items WHERE library_id=?1",
        ["query-library"],
    )
    .unwrap();
    for table in ["library_protocol_terms", "library_protocol_people"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    db.execute(
        "DELETE FROM libraries WHERE library_id=?1",
        ["query-library"],
    )
    .unwrap();
    db.execute(
        "DELETE FROM library_operations WHERE operation_id=?1",
        ["query-operation"],
    )
    .unwrap();
    for table in ["library_query_counts", "library_query_count_rebuild"] {
        let count: i64 = db
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE member_count=?1"),
                params![1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
