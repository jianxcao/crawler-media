use super::*;

fn library(store: &Store, root: &Path, name: &str) -> store::Library {
    std::fs::create_dir_all(root).unwrap();
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
}

fn request(store: &Store, library: &str, root: &Path, operation: &str) -> LibraryReconfiguration {
    LibraryReconfiguration {
        operation_id: operation.into(),
        library_id: library.parse().unwrap(),
        expected_revision: store.library_config_revision(library).unwrap(),
        root_paths: Some(vec![root.into()]),
        display_locale: None,
    }
}

#[test]
fn conflicting_root_is_rejected_before_freeze() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let a = library(&store, &temp.path().join("a"), "A");
    let b = library(&store, &temp.path().join("b"), "B");
    let revision = store.library_config_revision(&a.id).unwrap();
    let prepared = request(&store, &a.id, &b.root_paths[0], "root-conflict")
        .prepare()
        .unwrap();

    let result = store.begin_library_reconfiguration_prepared(&prepared, 1_000);

    assert!(
        matches!(result, Err(store::StoreError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(store.library_lifecycle_state(&a.id).unwrap(), "ready");
    assert_eq!(store.library_config_revision(&a.id).unwrap(), revision);
    assert!(store.active_library_operation(&a.id).unwrap().is_none());
    assert_eq!(
        store.get_library(&b.id).unwrap().unwrap().root_paths,
        b.root_paths
    );
}

#[test]
fn pending_and_failed_roots_reserve_keys_across_all_writes_and_restart() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let data = temp.path().join("data");
    let store = Store::open(&data).unwrap();
    let a = library(&store, &temp.path().join("a"), "A");
    let b = library(&store, &temp.path().join("b"), "B");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    store
        .begin_library_reconfiguration(&request(&store, &a.id, &target, "reserved"), 1)
        .unwrap();
    assert_reservation(&store, &b.id, &target);
    assert!(
        store
            .fail_library_operation_if_owned("reserved", "interrupted", 2)
            .unwrap()
    );
    assert_reservation(&store, &b.id, &target);
    drop(store);
    let store = Store::open(&data).unwrap();
    assert_reservation(&store, &b.id, &target);
    assert_eq!(
        store
            .advance_library_operation("reserved", 3, 100)
            .unwrap()
            .state,
        "succeeded"
    );
    // The old root is released only after its owner has committed the replacement.
    library(&store, &a.root_paths[0], "Released");
}

fn assert_reservation(store: &Store, other: &str, target: &Path) {
    let name = store.get_library(other).unwrap().unwrap().name;
    let roots = [target.to_str().unwrap()];
    assert!(matches!(
        store.create_library(MediaKind::Movie, "Rejected", &roots, "everyone", true, &[]),
        Err(store::StoreError::Conflict(_))
    ));
    assert!(
        !store
            .list_libraries()
            .unwrap()
            .iter()
            .any(|library| library.name == "Rejected")
    );
    assert!(matches!(
        store.insert_library_root(MediaKind::Movie, roots[0]),
        Err(store::StoreError::Conflict(_))
    ));
    assert!(matches!(
        store.update_library_full(other, Some("Rejected"), Some(&roots), None, None, None),
        Err(store::StoreError::Conflict(_))
    ));
    assert_eq!(store.get_library(other).unwrap().unwrap().name, name);
    let result = store.begin_library_reconfiguration(&request(store, other, target, "loser"), 3);
    assert!(
        matches!(result, Err(store::StoreError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(store.library_lifecycle_state(other).unwrap(), "ready");
}

#[test]
fn concurrently_prepared_root_operations_have_one_owner() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let store = std::sync::Arc::new(Store::open(temp.path().join("data")).unwrap());
    let a = library(&store, &temp.path().join("a"), "A");
    let b = library(&store, &temp.path().join("b"), "B");
    let target = temp.path().join("target");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads = [a, b]
        .into_iter()
        .enumerate()
        .map(|(index, library)| {
            let prepared = request(&store, &library.id, &target, &format!("owner-{index}"))
                .prepare()
                .unwrap();
            let barrier = barrier.clone();
            let store = store.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let result = store.begin_library_reconfiguration_prepared(&prepared, 1);
                (library.id, result)
            })
        })
        .collect::<Vec<_>>();
    let results = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let (loser, result) = results.iter().find(|(_, result)| result.is_err()).unwrap();
    assert!(matches!(result, Err(store::StoreError::Conflict(_))));
    assert_eq!(store.library_lifecycle_state(loser).unwrap(), "ready");
}

#[test]
fn aliases_conflict_but_own_and_independent_nested_roots_are_allowed() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let real = temp.path().join("real");
    let a = library(&store, &real, "A");
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let b = library(&store, &temp.path().join("b"), "B");
    assert!(matches!(
        store.begin_library_reconfiguration(&request(&store, &b.id, &alias, "alias-conflict"), 1),
        Err(store::StoreError::Conflict(_))
    ));
    store
        .begin_library_reconfiguration(&request(&store, &a.id, &alias, "own-alias"), 2)
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation("own-alias", 3, 100)
            .unwrap()
            .state,
        "succeeded"
    );
    library(&store, &real.join("nested"), "Independent nested");
}
