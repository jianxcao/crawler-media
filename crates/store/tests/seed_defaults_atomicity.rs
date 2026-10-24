use domain::MediaKind;
use rusqlite::Connection;
use store::Store;

#[test]
fn opening_after_a_failed_default_root_insert_retries_the_whole_new_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    drop(Store::open(&data).unwrap());
    let db = Connection::open(data.join("library.db")).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
         DELETE FROM library_roots WHERE library_id IN
             (SELECT library_id FROM libraries WHERE kind='tv');
         DELETE FROM libraries WHERE kind='tv';
         CREATE TRIGGER fail_seed_root BEFORE INSERT ON library_roots
         WHEN (SELECT kind FROM libraries WHERE library_id=NEW.library_id)='tv'
         BEGIN SELECT RAISE(ABORT, 'injected root failure'); END;",
    )
    .unwrap();

    assert!(Store::open(&data).is_err());
    let partial_libraries: i64 = db
        .query_row("SELECT count(*) FROM libraries WHERE kind='tv'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        partial_libraries, 0,
        "a rejected default root must roll back its Library"
    );

    db.execute_batch("DROP TRIGGER fail_seed_root;").unwrap();
    let store = Store::open(&data).unwrap();
    let library = store.default_library(MediaKind::Tv).unwrap().unwrap();
    assert_eq!(library.root_paths, vec![data.join("library/tv")]);
    let before = store.list_library_roots().unwrap();
    drop(store);
    assert_eq!(
        Store::open(&data).unwrap().list_library_roots().unwrap(),
        before
    );
}
