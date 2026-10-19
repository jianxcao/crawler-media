use domain::MediaKind;
use store::{Store, StoreError};

#[test]
fn frozen_removal_survives_restart_and_cleanup_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(&data).unwrap();
    let root = temp.path().join("extra");
    let library = store.create_library(MediaKind::Movie, "Extra", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let operation = store.begin_library_removal(&library.id).unwrap();
    drop(store);
    let store = Store::open(&data).unwrap();
    assert!(!store.library_accepts_work(&library.id).unwrap());
    assert_eq!(store.pending_library_removals().unwrap().len(), 1);
    store.set_removal_state(&operation.operation_id, "cleaning", None).unwrap();
    let first = store.clean_library_removal(&operation.operation_id).unwrap();
    assert_eq!(first.state, "succeeded");
    assert_eq!(store.clean_library_removal(&operation.operation_id).unwrap(), first);
    assert!(store.get_library(&library.id).unwrap().is_none());
}

#[test]
fn last_library_is_protected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let last = store.default_library(MediaKind::Movie).unwrap().unwrap();
    assert!(matches!(store.begin_library_removal(&last.id), Err(StoreError::Protected(_))));
}
