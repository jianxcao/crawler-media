//! R19: combined roots+locale changes and root passes that own rows with no file.

use std::path::{Path, PathBuf};

use domain::MediaKind;
use store::Store;
use store::library_operations::LibraryReconfiguration;

fn root_id_of(store: &Store, library_id: &str, path: &Path) -> String {
    store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library_id) && root.path == path)
        .unwrap_or_else(|| panic!("root {path:?} must exist"))
        .id
}

/// R19: one PATCH that changes both roots and locale commits both; the locale is never
/// silently dropped because a roots branch ran. An empty locale is refused before freezing.
#[test]
fn roots_and_locale_commit_together() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let requested_roots = vec![PathBuf::from("/tv"), PathBuf::from("/tv2")];

    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let refused = store.begin_library_reconfiguration(
        &LibraryReconfiguration {
            operation_id: uuid::Uuid::new_v4().to_string(),
            library_id: lib_id,
            expected_revision,
            root_paths: Some(requested_roots.clone()),
            display_locale: Some("  ".into()),
        },
        1_000,
    );
    assert!(refused.is_err());
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");

    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: Some(requested_roots.clone()),
                display_locale: Some("en-US".into()),
            },
            2_000,
        )
        .unwrap();
    let mut status = store.advance_library_operation(&operation_id, 2_100, 100).unwrap();
    for step in 0..10 {
        if status.state == "succeeded" {
            break;
        }
        status = store.advance_library_operation(&operation_id, 2_200 + step, 100).unwrap();
    }
    assert_eq!(status.state, "succeeded");
    let finished = store.get_library(&library.id).unwrap().unwrap();
    let finished_roots: Vec<PathBuf> = finished.root_paths.clone();
    assert_eq!(finished_roots, requested_roots);
    let finished_locale: String = rusqlite::Connection::open(tmp.path().join("library.db"))
        .unwrap()
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            rusqlite::params![library.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(finished_locale, "en-US");
}

/// R19: an unidentified row with no file under it still moves to its new root.
#[test]
fn unidentified_rows_without_files_move_to_the_new_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let old_root = root_id_of(&store, &library.id, Path::new("/tv"));
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    for path in ["/tv/anime/Unknown A", "/tv/anime/Unknown B", "/tv/Other"] {
        db.execute(
            "INSERT INTO unidentified (library_id, root_id, path, confidence)
             VALUES (?1, ?2, ?3, 'low')",
            rusqlite::params![library.id, old_root, path],
        )
        .unwrap();
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    let requested = vec![PathBuf::from("/tv"), PathBuf::from("/tv/anime")];
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision: store.library_config_revision(&library.id).unwrap(),
                root_paths: Some(requested),
                display_locale: None,
            },
            1_000,
        )
        .unwrap();
    let mut state = String::new();
    for step in 0..20 {
        state = store.advance_library_operation(&operation_id, 1_100 + step, 1).unwrap().state;
        if state == "succeeded" {
            break;
        }
    }
    assert_eq!(state, "succeeded");
    let anime = root_id_of(&store, &library.id, Path::new("/tv/anime"));
    let owner = |path: &str| -> String {
        db.query_row(
            "SELECT root_id FROM unidentified WHERE path = ?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(owner("/tv/anime/Unknown A"), anime);
    assert_eq!(owner("/tv/anime/Unknown B"), anime);
    assert_eq!(owner("/tv/Other"), old_root);
}
