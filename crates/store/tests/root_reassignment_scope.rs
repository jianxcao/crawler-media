//! Root reassignment remains atomic when Library creation borrows an outer SQL scope.
use domain::{Media, MediaId, MediaKind};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use store::{
    Db, PreparedLibraryRoots, Store,
    test_support::{FixtureFile, publish_fixture_file},
};

#[test]
fn rejected_initial_root_batch_preserves_its_outer_library_and_original_ownership() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("parent");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
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
    let roots = PreparedLibraryRoots::observe(&[nested.to_str().unwrap()]).unwrap();
    store
        .with_writer(Db::Library, |conn| {
            conn.authorizer(Some(|context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Update {
                        table_name: "library_file_units",
                        column_name: "library_id",
                        ..
                    }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
            Ok(())
        })
        .unwrap();
    // The HTTP creation path groups the Library and its settings in this scope.
    // Denial occurs after the file ownership UPDATE, inside the initial batch.
    let target = store.library_write(|library| {
        library.create_library_full_prepared(
            MediaKind::Movie,
            "Nested",
            &roots,
            "everyone",
            true,
            &[],
            &[],
            None,
        )
    });
    store
        .with_writer(Db::Library, |conn| {
            conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            Ok(())
        })
        .unwrap();

    let target = target.expect(
        "an initial reassignment failure must roll back its batch, so the outer Library creation can commit",
    );
    assert!(store.get_library(&target.id).unwrap().is_some());
    assert_eq!(
        store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .library_id
            .to_string(),
        source.id,
        "the failed batch must undo the earlier file ownership update",
    );
    let failed = store
        .root_reassignments_for_library(&target.id)
        .unwrap()
        .remove(0);
    assert_eq!(failed.state, "failed");
    assert!(failed.error.as_deref().unwrap().contains("not authorized"));
    store.drain_root_reassignments().unwrap();
    assert_eq!(
        store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .library_id
            .to_string(),
        target.id,
        "the retained reassignment retries after the writer fault is removed",
    );
}
