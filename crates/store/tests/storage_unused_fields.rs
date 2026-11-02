use rusqlite::Connection;
use store::Store;

#[test]
fn current_library_schema_excludes_unused_snapshot_and_removal_fields() {
    let temp = tempfile::tempdir().unwrap();
    let _store = Store::open(temp.path()).unwrap();
    let conn = Connection::open(temp.path().join("library.db")).unwrap();
    for (table, removed) in [
        ("library_removals", "removed_ledger_count"),
        ("media_metadata_sources", "preferences_json"),
    ] {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!columns.is_empty(), "{table} must exist");
        assert!(
            !columns.iter().any(|column| column == removed),
            "{table}.{removed} must be absent"
        );
    }
}

#[test]
fn library_removal_status_excludes_obsolete_ledger_counter() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("library");
    std::fs::create_dir(&root).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let operation = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&operation.operation_id, "cleaning", None)
        .unwrap();
    store
        .clean_library_removal(&operation.operation_id)
        .unwrap();
    let status = store
        .library_removal_status(&operation.operation_id)
        .unwrap()
        .unwrap();
    let response = serde_json::to_value(status).unwrap();
    assert_eq!(response["state"], "succeeded");
    assert!(response.get("removed_ledger_count").is_none());
    assert!(store.get_library(&library.id).unwrap().is_none());
}
