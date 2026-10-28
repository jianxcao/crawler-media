//! Demotion keeps recorded identity, facts and summaries in one transaction.
use domain::{LibraryFile, Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{
    Store,
    library_write::{WriteScope, demote::FileDemotion},
};

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Store,
    file: LibraryFile,
    scope: WriteScope,
}
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Demotion".into(),
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
    store
        .create_library(
            media.kind,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("Demotion.mkv");
    std::fs::write(&path, b"keep these bytes").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
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
    Fixture {
        _tmp: tmp,
        store,
        file,
        scope,
    }
}

#[test]
fn demotion_retires_published_facts_and_parks_the_recorded_owner_without_moving_bytes() {
    let f = fixture();
    assert_eq!(
        f.store
            .demote_files_to_unidentified(&[FileDemotion {
                scope: f.scope,
                file: f.file.clone()
            }])
            .unwrap(),
        1
    );
    assert!(
        f.store
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_none()
    );
    let parked = f
        .store
        .get_unidentified(f.file.path.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(parked.library_id, f.file.library_id.to_string());
    assert_eq!(parked.root_id, f.file.root_id);
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"keep these bytes");
    assert!(f.store.get_media(f.file.media_id).unwrap().is_some());
}

#[test]
fn renamed_file_cannot_be_demoted_using_its_previous_context() {
    let f = fixture();
    let destination = f.file.path.with_file_name("renamed.mkv");
    f.store
        .rename_file_within_root(f.file.file_id, &f.file.version, &f.file.path, &destination)
        .unwrap();
    assert!(
        f.store
            .demote_files_to_unidentified(&[FileDemotion {
                scope: f.scope,
                file: f.file.clone()
            }])
            .is_err()
    );
    assert!(f.store.list_unidentified(None).unwrap().is_empty());
    assert_eq!(
        f.store
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .path,
        destination
    );
}

#[test]
fn cleanup_failure_rolls_back_every_parked_file_and_published_row_in_the_batch() {
    let f = fixture();
    let other = publish_fixture_file(
        &f.store,
        &FixtureFile {
            media_id: f.file.media_id,
            path: f
                .file
                .path
                .with_file_name("other.mkv")
                .display()
                .to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let conn = rusqlite::Connection::open(f._tmp.path().join("data/library.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_demotion BEFORE DELETE ON library_files
        BEGIN SELECT RAISE(ABORT,'fixture rejects deletion'); END;",
    )
    .unwrap();
    assert!(
        f.store
            .demote_files_to_unidentified(&[
                FileDemotion {
                    scope: f.scope.clone(),
                    file: f.file.clone()
                },
                FileDemotion {
                    scope: f.scope,
                    file: other.clone()
                },
            ])
            .is_err()
    );
    assert!(f.store.list_unidentified(None).unwrap().is_empty());
    for file in [f.file, other] {
        assert!(f.store.file_coverage_by_id(file.file_id).unwrap().is_some());
    }
}

#[test]
fn demotion_refuses_a_transfer_reservation_and_keeps_the_whole_batch_unchanged() {
    let f = fixture();
    let mut candidate = f
        .store
        .file_coverage_by_id(f.file.file_id)
        .unwrap()
        .unwrap();
    candidate.file.file_id = domain::LedgerId::new();
    candidate.file.path = f.file.path.with_file_name("reserved.mkv");
    candidate.path_key = candidate.file.path.display().to_string();
    let pending = f
        .store
        .reserve_transfer(
            &f.scope,
            &candidate,
            &f.file.path,
            library::TransferMode::Copy,
            1000,
        )
        .unwrap();
    assert_eq!(pending.file.publication, domain::FilePublication::Pending);
    let result = f.store.demote_files_to_unidentified(&[
        FileDemotion {
            scope: f.scope.clone(),
            file: f.file.clone(),
        },
        FileDemotion {
            scope: f.scope,
            file: pending.file.clone(),
        },
    ]);
    assert!(
        matches!(result, Err(store::StoreError::Protected(_))),
        "pending placement cannot be demoted: {result:?}"
    );
    assert!(f.store.list_unidentified(None).unwrap().is_empty());
    for expected in [f.file, pending.file] {
        assert_eq!(
            f.store
                .file_coverage_by_id(expected.file_id)
                .unwrap()
                .unwrap()
                .file,
            expected
        );
    }
}

#[test]
fn demotion_refuses_an_active_rename_without_retiring_its_file_or_intent() {
    let f = fixture();
    let to = f.file.path.with_file_name("rename-destination.mkv");
    let rename = f
        .store
        .begin_file_rename(f.file.file_id, &f.file.version, &f.file.path, &to, 1000)
        .unwrap();
    let result = f.store.demote_files_to_unidentified(&[FileDemotion {
        scope: f.scope,
        file: f.file.clone(),
    }]);
    assert!(
        matches!(result, Err(store::StoreError::Protected(_))),
        "active rename cannot be demoted: {result:?}"
    );
    assert!(f.store.list_unidentified(None).unwrap().is_empty());
    assert_eq!(
        f.store
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .unwrap()
            .file,
        f.file
    );
    assert_eq!(f.store.load_file_rename(&rename.rename_id).unwrap(), rename);
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"keep these bytes");
    assert!(!to.exists());
}
