use super::*;
use store::library_operations::RootOperationRetryError;

fn fixture() -> (tempfile::TempDir, Store, store::Library) {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("a");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    (temp, store, library)
}

fn begin_failed(store: &Store, library: &store::Library) {
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: "correctable".into(),
                library_id: library.id.parse().unwrap(),
                expected_revision: 0,
                root_paths: Some(library.root_paths.clone()),
                display_locale: Some("en-US".into()),
            },
            1,
        )
        .unwrap();
    assert!(
        store
            .fail_library_operation_if_owned("correctable", "interrupted", 2)
            .unwrap()
    );
}

fn stored_plan(temp: &tempfile::TempDir) -> (String, Option<String>) {
    rusqlite::Connection::open(temp.path().join("data/library.db"))
        .unwrap()
        .query_row(
            "SELECT plan_json,cursor_json FROM library_operations WHERE operation_id='correctable'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn failed_conflicting_first_batch_can_replace_its_roots_and_finish_same_operation() {
    let (temp, store, a) = fixture();
    let b = store
        .create_library(
            MediaKind::Movie,
            "B",
            &[temp.path().join("b").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    begin_failed(&store, &a);
    // Reproduce an operation accepted before root admission became transactional.
    let (json, _) = stored_plan(&temp);
    let mut plan: serde_json::Value = serde_json::from_str(&json).unwrap();
    plan["roots"] =
        serde_json::json!([{"display_path":b.root_paths[0], "path_key":b.root_paths[0]}]);
    let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
    db.execute("UPDATE library_operations SET plan_json=?1,state='frozen' WHERE operation_id='correctable'", [plan.to_string()]).unwrap();
    assert!(matches!(
        store.advance_library_operation("correctable", 3, 100),
        Err(store::StoreError::Conflict(_))
    ));
    assert!(
        store
            .library_operation_view("correctable")
            .unwrap()
            .unwrap()
            .cursor
            .is_none()
    );
    // B's committed ownership wins over the historical conflicting plan from A.
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: "retaining-owner".into(),
                library_id: b.id.parse().unwrap(),
                expected_revision: 0,
                root_paths: Some(b.root_paths.clone()),
                display_locale: None,
            },
            4,
        )
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation("retaining-owner", 5, 100)
            .unwrap()
            .state,
        "succeeded"
    );
    let roots = store::PreparedLibraryRoots::observe(&[a.root_paths[0].to_str().unwrap()]).unwrap();
    let view = store
        .replace_failed_roots_operation("correctable", 1, &roots, 6)
        .unwrap();
    assert_eq!(view.operation_id, "correctable");
    assert_eq!(view.state, "failed");
    assert_eq!(store.library_config_revision(&a.id).unwrap(), 1);
    assert_eq!(
        store
            .advance_library_operation("correctable", 7, 100)
            .unwrap()
            .state,
        "succeeded"
    );
    assert_eq!(store.library_lifecycle_state(&a.id).unwrap(), "ready");
    assert_eq!(
        store.get_library(&a.id).unwrap().unwrap().root_paths,
        a.root_paths
    );
    assert_eq!(
        store::library_query::details::library_display_locale(&store, a.id.parse().unwrap())
            .unwrap()
            .as_deref(),
        Some("en-US")
    );
}

#[test]
fn rejected_replacement_preserves_plan_cursor_and_library_revision() {
    let (temp, store, a) = fixture();
    store
        .insert_unidentified(
            a.root_paths[0].join("unknown.mkv").display().to_string(),
            domain::Confidence::Low,
        )
        .unwrap();
    begin_failed(&store, &a);
    let covered =
        store::PreparedLibraryRoots::observe(&[a.root_paths[0].to_str().unwrap()]).unwrap();
    let outside = temp.path().join("outside");
    let uncovered = store::PreparedLibraryRoots::observe(&[outside.to_str().unwrap()]).unwrap();
    let before = stored_plan(&temp);
    assert!(matches!(
        store.replace_failed_roots_operation("correctable", 0, &covered, 3),
        Err(RootOperationRetryError::NotOwner)
    ));
    assert!(matches!(
        store.replace_failed_roots_operation("correctable", 1, &uncovered, 3),
        Err(RootOperationRetryError::Store(store::StoreError::Invalid(
            _
        )))
    ));
    assert_eq!(stored_plan(&temp), before);
    let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
    db.execute(
        "UPDATE library_operations SET cursor_json='{}' WHERE operation_id='correctable'",
        [],
    )
    .unwrap();
    let before = stored_plan(&temp);
    assert!(matches!(
        store.replace_failed_roots_operation("correctable", 1, &covered, 3),
        Err(RootOperationRetryError::NotReplannable)
    ));
    assert_eq!(stored_plan(&temp), before);
    assert_eq!(store.library_config_revision(&a.id).unwrap(), 1);
    // The persisted cursor still supports an ordinary retry.
    assert_eq!(
        store
            .advance_library_operation("correctable", 4, 100)
            .unwrap()
            .state,
        "succeeded"
    );
}

#[test]
fn corrupt_reserved_plan_refuses_new_root_without_mutating_requesting_library() {
    let (temp, store, a) = fixture();
    begin_failed(&store, &a);
    let b = store
        .create_library(
            MediaKind::Movie,
            "B",
            &[temp.path().join("b").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
    db.execute(
        "UPDATE library_operations SET plan_json=?1 WHERE operation_id='correctable'",
        [r#"{"roots":"invalid root list"}"#],
    )
    .unwrap();
    let result = store.begin_library_reconfiguration(
        &LibraryReconfiguration {
            operation_id: "corrupt-reservation".into(),
            library_id: b.id.parse().unwrap(),
            expected_revision: 0,
            root_paths: Some(vec![temp.path().join("free")]),
            display_locale: None,
        },
        3,
    );
    assert!(
        matches!(result, Err(store::StoreError::Invalid(message)) if message.contains("correctable") && message.contains("plan"))
    );
    assert_eq!(store.library_lifecycle_state(&b.id).unwrap(), "ready");
    assert_eq!(store.library_config_revision(&b.id).unwrap(), 0);
    assert!(store.active_library_operation(&b.id).unwrap().is_none());
}
