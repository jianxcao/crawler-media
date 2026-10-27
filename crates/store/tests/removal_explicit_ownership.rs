use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

fn movie(store: &Store) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Removal".into(),
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
    media.id
}

#[test]
#[cfg(unix)]
fn unrelated_library_disk_error_cannot_prevent_removal() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let target = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[a.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "B",
            &[b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = b.join("other.mkv");
    std::fs::write(&path, b"video").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: movie(&store),
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("other.mkv", &path).unwrap();
    let removal = store.begin_library_removal(&target.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 0);
    assert!(store.file_coverage_by_id(file.file_id).unwrap().is_some());
}

#[test]
fn removal_snapshot_and_cleanup_resume_in_finite_windows_and_validate_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let root = temp.path().join("target");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&data).unwrap();
    let target = store
        .create_library(
            MediaKind::Movie,
            "Target",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = movie(&store);
    for index in 0..301 {
        let path = root.join(format!("{index:03}.mkv"));
        std::fs::write(&path, b"keep").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let operation = store.begin_library_removal(&target.id).unwrap();
    let db = rusqlite::Connection::open(data.join("library.db")).unwrap();
    let count: usize = db
        .query_row(
            "SELECT count(*) FROM library_removal_files WHERE operation_id=?1",
            [&operation.operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 100, "freeze captures one finite snapshot window");
    drop(store);
    let store = Store::open(&data).unwrap();
    assert!(
        !store
            .capture_library_removal_batch(&operation.operation_id)
            .unwrap()
    );
    assert!(
        !store
            .capture_library_removal_batch(&operation.operation_id)
            .unwrap()
    );
    assert!(
        store
            .capture_library_removal_batch(&operation.operation_id)
            .unwrap()
    );
    store
        .set_removal_state(&operation.operation_id, "cleaning", None)
        .unwrap();
    db.execute(
        "UPDATE libraries SET config_revision=config_revision+1 WHERE library_id=?1",
        [&target.id],
    )
    .unwrap();
    assert!(
        store
            .clean_library_removal_batch(&operation.operation_id)
            .is_err()
    );
    assert_eq!(
        store::test_support::library_file_count(&store, &target.id),
        301
    );
    db.execute(
        "UPDATE libraries SET config_revision=config_revision-1 WHERE library_id=?1",
        [&target.id],
    )
    .unwrap();
    store
        .clean_library_removal_batch(&operation.operation_id)
        .unwrap();
    assert_eq!(
        store::test_support::library_file_count(&store, &target.id),
        201
    );
    drop(store);
    let store = Store::open(&data).unwrap();
    let done = store
        .clean_library_removal(&operation.operation_id)
        .unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 301);
    assert_eq!(std::fs::read(root.join("000.mkv")).unwrap(), b"keep");
}
