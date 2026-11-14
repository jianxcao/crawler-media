//! Borrowed mutations publish root identities only after the outer SQL commit.
use domain::MediaKind;
use store::{Db, PreparedLibraryRoots, Store, StoreError};

fn normalized(store: &Store, raw: &str) -> String {
    store
        .with_read(Db::Library, |conn| {
            Ok(conn.query_row("SELECT norm_path_key(?1)", [raw], |row| row.get(0))?)
        })
        .unwrap()
}

#[test]
fn rolled_back_library_roots_never_escape_to_other_readers() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let physical = dir.path().join("physical");
    let alias = dir.path().join("alias");
    std::fs::create_dir(&physical).unwrap();
    std::os::unix::fs::symlink(&physical, &alias).unwrap();
    let raw = alias.join("missing.mkv").to_string_lossy().into_owned();
    let expected = store::path_key::normalize_path_key(
        physical
            .canonicalize()
            .unwrap()
            .join("missing.mkv")
            .to_str()
            .unwrap(),
    );
    let before = normalized(&store, &raw);
    let prepared = PreparedLibraryRoots::observe(&[alias.to_str().unwrap()]).unwrap();
    let result: Result<(), StoreError> = store.library_write(|library| {
        let created = library.create_library_full_prepared(
            MediaKind::Movie,
            "Uncommitted",
            &prepared,
            "everyone",
            true,
            &[],
            &[],
            None,
        )?;
        assert!(library.get_library(&created.id)?.is_some());
        assert_eq!(
            library.recorded_path_key(&raw),
            expected,
            "borrowed mutation reads its own root"
        );
        assert_eq!(
            normalized(&store, &raw),
            before,
            "other readers cannot see an uncommitted root alias"
        );
        Err(StoreError::Protected("rollback fixture".into()))
    });
    assert!(result.is_err());
    let writer_key: String = store
        .with_writer(Db::Library, |conn| {
            Ok(conn.query_row("SELECT norm_path_key(?1)", [&raw], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(
        writer_key, before,
        "rollback restores the writer SQL alias function"
    );
    assert_eq!(
        normalized(&store, &raw),
        before,
        "rollback cannot leave a cached root alias"
    );
    store
        .library_write(|library| {
            library.create_library_full_prepared(
                MediaKind::Movie,
                "Committed",
                &prepared,
                "everyone",
                true,
                &[],
                &[],
                None,
            )?;
            assert_eq!(library.recorded_path_key(&raw), expected);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        normalized(&store, &raw),
        expected,
        "outer commit publishes the root alias"
    );
}
