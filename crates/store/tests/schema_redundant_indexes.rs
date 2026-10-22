use rusqlite::Connection;
use std::path::Path;
use store::Store;

#[test]
fn fresh_library_does_not_create_redundant_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    drop(Store::open(&data_dir).unwrap());

    let library_db = data_dir.join("library.db");
    let conn = Connection::open(&library_db).unwrap();

    let sessions_user_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = 'sessions_user')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !sessions_user_exists,
        "newly created library.db must not contain redundant index sessions_user"
    );

    let model_sample_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = 'model_sample')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !model_sample_exists,
        "newly created library.db must not contain redundant index model_sample"
    );
}
