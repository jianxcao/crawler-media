mod library_integrity_support;

use library_integrity_support::*;

#[test]
fn orphan_parents_share_the_target_budget_with_the_last_parents_children() {
    let fixture = fixture(100);
    many_derived_users(&fixture, 99);
    let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db.execute(
        "DELETE FROM user_library_continue_units WHERE episode<100",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE library_units SET available_file_count=0,preferred_file_id=NULL",
        [],
    )
    .unwrap();
    db.execute("DELETE FROM library_file_units", []).unwrap();
    fixture
        .store
        .begin_library_integrity(fixture.library, true, "budget", 0)
        .unwrap();
    reach_reverse(&fixture, "budget", 0, 1_000);
    let counts = || {
        db.query_row(
            "SELECT (SELECT count(*) FROM library_units),
            (SELECT count(*) FROM user_library_continue_units)",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap()
    };
    let before = counts();
    assert_eq!(before, (100, 100));
    fixture
        .store
        .advance_library_operation("budget", 2_000, 1)
        .unwrap();
    let after = counts();
    assert_eq!(
        before.0 - after.0 + before.1 - after.1,
        100,
        "one target page must share its budget between orphan parents and children"
    );
    assert_eq!(
        after.0, 1,
        "last parent remains until every child page is drained"
    );
    finish(&fixture.store, "budget");
    assert_eq!(counts(), (0, 0));
}
