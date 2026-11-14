//! Generic pools identify the database whose file was replaced.
use std::num::NonZeroUsize;

use rusqlite::Connection;
use store::{Db, LibraryReadError, ReaderPool, Store, StoreError};

fn temp_dir() -> tempfile::TempDir {
    std::fs::create_dir_all("/tmp/crawler-storage-fixes").unwrap();
    tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap()
}

#[test]
fn replaced_database_errors_name_the_actual_database_and_close_its_pool() {
    for (db, name) in [
        (Db::App, "app"),
        (Db::Subscribe, "subscribe"),
        (Db::Cache, "cache"),
    ] {
        let temp = temp_dir();
        let store = Store::open(temp.path()).unwrap();
        let pool = store.reader_pool(db);
        let path = temp.path().join(format!("{name}.db"));
        let replaced = path.with_extension("replaced");
        std::fs::rename(&path, &replaced).unwrap();
        std::fs::copy(&replaced, &path).unwrap();

        let error = pool.check_open().unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("{name}.db file identity changed")),
            "{error}"
        );
        assert!(matches!(pool.check_open(), Err(LibraryReadError::Closed)));
        assert_eq!(pool.available(), 0);
    }
}

fn replace_file_during_validation(connection: &Connection) -> Result<(), StoreError> {
    let path = std::path::PathBuf::from(connection.path().unwrap());
    let replaced = path.with_extension("replaced");
    std::fs::rename(&path, &replaced)?;
    std::fs::copy(&replaced, &path)?;
    Ok(())
}

#[test]
fn database_replacement_during_open_names_the_actual_database() {
    let temp = temp_dir();
    let _store = Store::open(temp.path()).unwrap();
    let result = ReaderPool::open(
        temp.path().join("app.db"),
        NonZeroUsize::new(1).unwrap(),
        "app",
        replace_file_during_validation,
    );
    let error = match result {
        Ok(_) => panic!("a replaced database cannot be admitted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("app.db changed during reader open"),
        "{error}"
    );
}
