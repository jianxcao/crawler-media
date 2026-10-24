use std::path::Path;

use domain::{Confidence, MediaKind};
use rusqlite::{Connection, params};
use store::Store;
use store::test_support::{pending_root_reassignments, set_root_reassign_batch_budget};

fn create_library(store: &Store, name: &str, root: &Path) -> String {
    store
        .create_library(
            MediaKind::Movie,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
}

fn count(db: &Connection, library: &str) -> usize {
    db.query_row(
        "SELECT count(*) FROM unidentified WHERE library_id=?1",
        [library],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn nested_library_reassigns_unidentified_without_files_in_resumable_batches() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let real = base.join("real");
    std::fs::create_dir_all(real.join("nested")).unwrap();
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let data = base.join("data");
    let store = Store::open(&data).unwrap();
    let parent = create_library(&store, "Parent", &alias);
    for index in 0..251 {
        store
            .insert_unidentified(
                alias
                    .join(format!("nested/u{index:03}"))
                    .display()
                    .to_string(),
                Confidence::Low,
            )
            .unwrap();
    }
    let db = Connection::open(data.join("library.db")).unwrap();
    set_root_reassign_batch_budget(Some(1));
    let nested = create_library(&store, "Nested", &alias.join("nested"));
    set_root_reassign_batch_budget(None);
    assert_eq!(count(&db, &nested), 100);
    assert_eq!(count(&db, &parent), 151);
    assert_eq!(pending_root_reassignments(&store).len(), 1);
    drop(store);

    let store = Store::open(&data).unwrap();
    store.drain_root_reassignments().unwrap();
    assert_eq!(count(&db, &nested), 251);
    assert_eq!(count(&db, &parent), 0);
    assert_eq!(store.list_unidentified().unwrap().len(), 251);
    assert!(pending_root_reassignments(&store).is_empty());
    let wrong_roots: i64 = db
        .query_row(
            "SELECT count(*) FROM unidentified u JOIN library_roots r ON r.root_id=u.root_id
         WHERE u.library_id=?1 AND (r.library_id<>u.library_id OR u.confidence<>'low')",
            params![nested],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(wrong_roots, 0);
}

#[test]
fn inserting_an_extra_root_reassigns_unidentified_to_its_default_library() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let parent_root = base.join("parent");
    let nested_root = parent_root.join("nested");
    std::fs::create_dir_all(&nested_root).unwrap();
    let data = base.join("data");
    let store = Store::open(&data).unwrap();
    let parent = create_library(&store, "Parent", &parent_root);
    let path = nested_root.join("unmatched").display().to_string();
    store
        .insert_unidentified(path.clone(), Confidence::Low)
        .unwrap();
    let root = store
        .insert_library_root(MediaKind::Movie, nested_root.to_str().unwrap())
        .unwrap();
    let db = Connection::open(data.join("library.db")).unwrap();
    let (library, root_id): (String, String) = db
        .query_row(
            "SELECT library_id,root_id FROM unidentified WHERE path=?1",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(library, parent);
    assert_eq!(Some(library), root.library_id);
    assert_eq!(root_id, root.id);
}

#[test]
fn filesystem_root_can_own_a_previously_parked_unidentified_path() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let path = tmp.path().join("outside-all-roots").display().to_string();
    store
        .insert_unidentified(path.clone(), Confidence::Low)
        .unwrap();
    let root = store.insert_library_root(MediaKind::Movie, "/").unwrap();
    let db = Connection::open(data.join("library.db")).unwrap();
    let owner: String = db
        .query_row(
            "SELECT root_id FROM unidentified WHERE path=?1",
            [path],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner, root.id);
}
