use domain::{LibraryFile, Media, MediaId, MediaKind, UnitKey};
use store::{
    Store,
    library_write::{FilePublication, WriteScope},
};

fn fixture() -> (tempfile::TempDir, Store, LibraryFile, WriteScope) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "M",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "M".into(),
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
    let path = root.join("movie.mkv");
    std::fs::write(&path, b"first").unwrap();
    let file = store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    (tmp, store, file, scope)
}

fn queue(store: &Store, file: &LibraryFile) {
    let path = file.path.to_str().unwrap();
    store
        .enqueue_file_deletion_observed(path, "test", store::file_identity(path))
        .unwrap();
}

#[test]
fn pending_deletion_freezes_source_publication_and_rename() {
    let (_tmp, store, file, scope) = fixture();
    queue(&store, &file);
    let mut observation = library::observe_source(&file.path).unwrap();
    observation.source_version.push_str("-next");
    let replaced =
        store.replace_file_source(&scope, file.file_id, &file.version, &observation, 100);
    assert!(
        matches!(replaced, Err(store::StoreError::Protected(_))),
        "{replaced:?}"
    );
    let mut changed = file.clone();
    changed.version.source_version = observation.source_version;
    let published = store.publish_files(
        &scope,
        &[FilePublication {
            file: &changed,
            units: &[UnitKey::WHOLE],
            expected_version: Some(&file.version),
            nfo_observation: None,
            probe_required: false,
        }],
        100,
    );
    assert!(
        matches!(published, Err(store::StoreError::Protected(_))),
        "{published:?}"
    );
    let to = file.path.with_file_name("renamed.mkv");
    assert!(
        store
            .begin_file_rename(file.file_id, &file.version, &file.path, &to, 100)
            .is_err()
    );
    assert_eq!(
        store.get_file_version(file.file_id).unwrap().unwrap(),
        file.version
    );
    assert_eq!(std::fs::read(&file.path).unwrap(), b"first");
    store
        .abandon_file_deletion(&store.list_due_file_deletions(0).unwrap()[0])
        .unwrap();
    assert!(
        store
            .replace_file_source(
                &scope,
                file.file_id,
                &file.version,
                &library::observe_source(&file.path).unwrap(),
                101
            )
            .is_ok()
    );
}

#[test]
fn deletion_also_blocks_new_file_identity_at_the_same_path() {
    let (_tmp, store, file, scope) = fixture();
    queue(&store, &file);
    store
        .delete_ledger_path(file.path.to_str().unwrap())
        .unwrap();
    let mut replacement = file.clone();
    replacement.file_id = domain::LedgerId::new();
    let result = store.publish_files(
        &scope,
        &[FilePublication {
            file: &replacement,
            units: &[UnitKey::WHOLE],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        }],
        100,
    );
    assert!(
        matches!(result, Err(store::StoreError::Protected(_))),
        "{result:?}"
    );
    assert!(
        store
            .file_coverage_by_id(replacement.file_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(std::fs::read(&file.path).unwrap(), b"first");
}

#[test]
fn path_aliases_share_one_deletion_owner_and_lookup_stays_bounded() {
    let (tmp, store, file, _scope) = fixture();
    queue(&store, &file);
    let alias = file.path.parent().unwrap().join(".").join("movie.mkv");
    store
        .enqueue_file_deletion_observed(
            alias.to_str().unwrap(),
            "alias",
            store::file_identity(alias.to_str().unwrap()),
        )
        .unwrap();
    assert_eq!(store.list_due_file_deletions(0).unwrap().len(), 1);
    let (before, small) = store.with_stats(store::stats::StatsScope::Library, || {
        store
            .due_file_deletion(alias.to_str().unwrap(), 0)
            .unwrap()
            .unwrap()
    });
    let conn = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    for n in 0..10_000 {
        let path = format!("/unrelated/{n}.mkv");
        conn.execute("INSERT INTO pending_file_deletions(path,path_key,deletion_id,reason) VALUES (?1,?1,?1,'noise')",[path]).unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
    let (after, large) = store.with_stats(store::stats::StatsScope::Library, || {
        store
            .due_file_deletion(alias.to_str().unwrap(), 0)
            .unwrap()
            .unwrap()
    });
    assert_eq!(after, before);
    assert_eq!(large.statements, 1);
    assert_eq!(large.vm_steps, small.vm_steps);
    assert!(
        store
            .file_deletion_pending(alias.to_str().unwrap())
            .unwrap()
    );
}

#[test]
fn new_intent_has_a_new_generation_even_when_native_identity_is_unchanged() {
    let (_tmp, store, file, _scope) = fixture();
    queue(&store, &file);
    let original = store
        .due_file_deletion(file.path.to_str().unwrap(), 0)
        .unwrap()
        .unwrap();
    queue(&store, &file);
    assert_eq!(
        store
            .due_file_deletion(file.path.to_str().unwrap(), 0)
            .unwrap()
            .unwrap(),
        original
    );
    store
        .abandon_file_deletion(&store.list_due_file_deletions(0).unwrap()[0])
        .unwrap();
    queue(&store, &file);
    let new = store
        .due_file_deletion(file.path.to_str().unwrap(), 0)
        .unwrap()
        .unwrap();
    assert_ne!(new.deletion_id, original.deletion_id);
    assert_eq!(
        (new.file_id.as_ref(), new.file_inode, new.file_size),
        (
            original.file_id.as_ref(),
            original.file_inode,
            original.file_size
        )
    );
    store
        .record_file_deletion_failure(&original, 0, "stale worker")
        .unwrap();
    store.abandon_file_deletion(&original).unwrap();
    store.complete_file_deletion(&original).unwrap();
    assert_eq!(store.due_file_deletion_intent(&new, 0).unwrap(), Some(new));
    assert!(store.file_coverage_by_id(file.file_id).unwrap().is_some());
    assert_eq!(std::fs::read(&file.path).unwrap(), b"first");
}
