mod query_model_support;
use query_model_support::*;
use rusqlite::{Connection, params};

#[test]
fn integrity_detects_and_repairs_query_projection_corruption() {
    let (tmp, store, lib, ids) = setup();
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE library_query_counts SET member_count=999 WHERE dimension='genre'",
        [],
    )
    .unwrap();
    db.execute(
        "DELETE FROM library_protocol_terms WHERE media_id=?1",
        [&ids[0]],
    )
    .unwrap();
    db.execute(
        "UPDATE library_items SET query_projection_revision=-1 WHERE media_id=?1",
        [&ids[1]],
    )
    .unwrap();
    run(&store, lib, false, "check");
    assert!(
        store
            .library_integrity_report("check")
            .unwrap()
            .totals
            .query_projection_mismatches
            > 0
    );
    run(&store, lib, true, "repair");
    run(&store, lib, false, "verify");
    assert_eq!(
        store
            .library_integrity_report("verify")
            .unwrap()
            .totals
            .query_projection_mismatches,
        0
    );
    let count: i64 = db.query_row("SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='genre' AND value='Drama'", params![lib.to_string()], |r|r.get(0)).unwrap();
    assert_eq!(count, 2);
}

#[test]
fn integrity_repairs_undercount_with_extra_members() {
    for damage in [
        "UPDATE library_query_counts SET member_count=0",
        "UPDATE library_query_counts SET member_count=1",
        "DELETE FROM library_query_counts",
    ] {
        let (tmp, store, lib, ids) = setup();
        let db = Connection::open(tmp.path().join("library.db")).unwrap();
        db.execute(damage, []).unwrap();
        db.execute("INSERT INTO library_item_terms(library_id,media_id,term_kind,term_value) VALUES (?1,?2,'genre','Extra')", params![lib.to_string(), ids[0]]).unwrap();
        run(&store, lib, false, "check");
        assert!(
            store
                .library_integrity_report("check")
                .unwrap()
                .has_issues()
        );
        let extra: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM library_item_terms WHERE term_value='Extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(extra, 1, "check is read-only for derived rows");
        run(&store, lib, true, "repair");
        drop(store);
        let store = store::Store::open(tmp.path()).unwrap();
        run(&store, lib, false, "verify");
        assert!(
            !store
                .library_integrity_report("verify")
                .unwrap()
                .has_issues()
        );
    }
}
