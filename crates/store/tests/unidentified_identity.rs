use domain::{Confidence, MediaKind};
use rusqlite::Connection;
use store::{
    Store,
    library_write::{FilePublication, WriteScope},
};

#[path = "support/mod.rs"]
mod support;

/// Park a published file as unidentified while its claim target stays: deleting
/// the last file purges the orphan media row, and a claim always targets an
/// existing media.
fn detach_keeping_media(store: &Store, path: &str, media_id: domain::MediaId) {
    let media = store.get_media(media_id).unwrap().expect("seeded media");
    store.delete_ledger_path(path).unwrap();
    if store.get_media(media_id).unwrap().is_none() {
        store.insert_media(&media).unwrap();
    }
}

#[test]
fn claim_consumes_only_observed_source_and_rolls_back_with_failed_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let root = tmp.path().join("movies");
    let (_, id) = support::seed_published_file(&store, &root, "Claim");
    let file = store.file_coverage_by_id(id).unwrap().unwrap().file;
    let path = file.path.display().to_string();
    detach_keeping_media(&store, &path, file.media_id);
    store
        .insert_unidentified(path.clone(), Confidence::Low)
        .unwrap();
    let source = store.get_unidentified(&path).unwrap().unwrap();
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    let publication = FilePublication {
        file: &file,
        units: &[domain::UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    store
        .insert_unidentified(path.clone(), Confidence::High)
        .unwrap();
    assert!(
        store
            .publish_unidentified_checked_context(&scope, &publication, None, &source, 1)
            .is_err()
    );
    assert!(store.file_coverage_by_id(id).unwrap().is_none());
    let source = store.get_unidentified(&path).unwrap().unwrap();
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_claim BEFORE INSERT ON library_files BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        store
            .publish_unidentified_checked_context(&scope, &publication, None, &source, 2)
            .is_err()
    );
    assert_eq!(store.get_unidentified(&path).unwrap().unwrap(), source);
    db.execute_batch("DROP TRIGGER fail_claim").unwrap();
    store
        .publish_unidentified_checked_context(&scope, &publication, None, &source, 3)
        .unwrap();
    assert!(store.get_unidentified(&path).unwrap().is_none());
    assert_eq!(
        store
            .file_coverage_by_id(id)
            .unwrap()
            .unwrap()
            .file
            .library_id,
        file.library_id
    );
}

#[test]
fn unidentified_seek_survives_reopen_and_insert_before_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("parent");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Parent",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    for i in 0..351 {
        store
            .insert_unidentified(
                root.join(format!("nested/p{i:03}.mkv"))
                    .display()
                    .to_string(),
                Confidence::Low,
            )
            .unwrap();
    }
    store::test_support::set_root_reassign_batch_budget(Some(1));
    let nested = store
        .create_library(
            MediaKind::Tv,
            "Nested",
            &[root.join("nested").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store::test_support::set_root_reassign_batch_budget(None);
    drop(store);
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_unidentified(
            root.join("nested/a-before-cursor.mkv")
                .display()
                .to_string(),
            Confidence::Low,
        )
        .unwrap();
    let owned_before = store.list_unidentified(Some(&nested.id)).unwrap().len();
    store.drain_one_root_batch().unwrap();
    assert_eq!(
        store.list_unidentified(Some(&nested.id)).unwrap().len(),
        owned_before + 100,
        "correct new facts before the cursor must not rewind already completed work"
    );
    store.drain_root_reassignments().unwrap();
    let rows = store.list_unidentified(Some(&nested.id)).unwrap();
    assert_eq!(rows.len(), 352);
    assert!(rows.iter().all(|row| row.library_id == nested.id));
    assert!(store::test_support::pending_root_reassignments(&store).is_empty());
}

#[test]
fn an_old_claim_cannot_consume_a_deleted_and_reparked_source() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (_, id) = support::seed_published_file(&store, &tmp.path().join("movies"), "Repark");
    let mut file = store.file_coverage_by_id(id).unwrap().unwrap().file;
    let path = file.path.display().to_string();
    detach_keeping_media(&store, &path, file.media_id);
    store
        .insert_unidentified(path.clone(), Confidence::Low)
        .unwrap();
    let old_source = store.get_unidentified(&path).unwrap().unwrap();
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    let units = [domain::UnitKey::WHOLE];
    let publication = FilePublication {
        file: &file,
        units: &units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    // Another successful claim consumes the observed row, then an explicit
    // re-identification detaches this path and parks the replacement again.
    store
        .publish_unidentified_checked_context(&scope, &publication, None, &old_source, 1)
        .unwrap();
    detach_keeping_media(&store, &path, file.media_id);
    std::fs::write(&file.path, b"replacement source bytes").unwrap();
    store
        .insert_unidentified(path.clone(), Confidence::Low)
        .unwrap();
    let replacement = store.get_unidentified(&path).unwrap().unwrap();

    file.file_id = domain::LedgerId::new();
    file.path = file.path.parent().unwrap().join("Delayed.mkv");
    std::fs::write(&file.path, b"staged old source").unwrap();
    file.version.source_version = library::current_source_version(&file.path);
    let publication = FilePublication {
        file: &file,
        units: &units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    assert!(matches!(
        store.publish_unidentified_checked_context(&scope, &publication, None, &old_source, 2,),
        Err(store::StoreError::Protected(_))
    ));
    assert_eq!(store.get_unidentified(&path).unwrap().unwrap(), replacement);
    assert!(store.file_coverage_by_id(file.file_id).unwrap().is_none());
    assert_eq!(std::fs::read(path).unwrap(), b"replacement source bytes");
}
