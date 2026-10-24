use domain::{Media, MediaId, MediaKind};
use store::{
    Store,
    test_support::{FixtureFile, publish_fixture_file},
};

#[test]
fn failed_reassignment_keeps_its_identity_error_and_files_until_retry_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let root = temp.path().join("parent");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(&data).unwrap();
    let source = store
        .create_library(
            MediaKind::Movie,
            "Parent",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "One".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = nested.join("one.mkv");
    std::fs::write(&path, b"video").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let db = rusqlite::Connection::open(data.join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_reassignment BEFORE UPDATE OF library_id ON library_files BEGIN SELECT RAISE(ABORT,'injected root failure'); END;").unwrap();
    let target = store
        .create_library(
            MediaKind::Movie,
            "Nested",
            &[nested.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let failed = store
        .root_reassignments_for_library(&target.id)
        .unwrap()
        .remove(0);
    assert_eq!(failed.state, "failed");
    assert!(
        failed
            .error
            .as_deref()
            .unwrap()
            .contains("injected root failure")
    );
    assert_eq!(
        store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .library_id
            .to_string(),
        source.id
    );
    drop(store);
    db.execute_batch("DROP TRIGGER fail_reassignment").unwrap();
    let store = Store::open(&data).unwrap();
    store.drain_root_reassignments().unwrap();
    let finished = store
        .root_reassignments_for_library(&target.id)
        .unwrap()
        .remove(0);
    assert_eq!(finished.operation_id, failed.operation_id);
    assert_eq!(finished.state, "succeeded");
    assert!(finished.revision > failed.revision);
    assert!(finished.error.is_none());
    assert_eq!(
        store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .library_id
            .to_string(),
        target.id
    );
}
