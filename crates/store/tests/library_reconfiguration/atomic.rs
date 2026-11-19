use super::*;

#[test]
fn stale_preparation_rolls_back_settings_even_after_the_winner_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Original",
            &["/atomic"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&library.id, true, true)
        .unwrap();
    let request = |operation: &str| LibraryReconfiguration {
        operation_id: operation.into(),
        library_id: library.id.parse().unwrap(),
        expected_revision: 0,
        root_paths: None,
        display_locale: Some("en-US".into()),
    };
    let loser = request("loser").prepare().unwrap();
    store
        .begin_library_reconfiguration(&request("winner"), 1)
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation("winner", 2, 100)
            .unwrap()
            .state,
        "succeeded"
    );
    let error = store
        .reconfigure_library_with(&loser, 3, |db| {
            db.set_library_intro_settings(&library.id, false, false)?;
            db.update_library_settings(
                &library.id,
                Some("Loser"),
                Some(("selected", false, &[])),
                None,
                None,
            )
        })
        .unwrap_err();
    assert!(matches!(error, store::StoreError::Protected(_)));
    let current = store.get_library(&library.id).unwrap().unwrap();
    assert_eq!(current.name, "Original");
    assert_eq!(current.access_mode, "everyone");
    assert!(current.admin_visible && current.detect_intros && current.enable_fingerprint);
    assert_eq!(store.library_config_revision(&library.id).unwrap(), 1);
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
    assert!(store.get_library_operation("loser").unwrap().is_none());
}

#[test]
fn settings_and_frozen_operation_survive_reopen_together() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Original",
            &["/atomic"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let prepared = LibraryReconfiguration {
        operation_id: "atomic".into(),
        library_id: library.id.parse().unwrap(),
        expected_revision: 0,
        root_paths: None,
        display_locale: Some("en-US".into()),
    }
    .prepare()
    .unwrap();
    store
        .reconfigure_library_with(&prepared, 1, |db| {
            db.set_library_intro_settings(&library.id, true, true)?;
            db.update_library_settings(&library.id, Some("Committed"), None, None, None)
        })
        .unwrap();
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    let current = store.get_library(&library.id).unwrap().unwrap();
    assert_eq!(current.name, "Committed");
    assert!(current.detect_intros && current.enable_fingerprint);
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );
    assert_eq!(
        store
            .get_library_operation("atomic")
            .unwrap()
            .unwrap()
            .state,
        "frozen"
    );
    assert_eq!(
        store
            .advance_library_operation("atomic", 2, 100)
            .unwrap()
            .state,
        "succeeded"
    );
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
    assert_eq!(
        store::library_query::library_display_locale(&store, library.id.parse().unwrap())
            .unwrap()
            .as_deref(),
        Some("en-US")
    );
}
