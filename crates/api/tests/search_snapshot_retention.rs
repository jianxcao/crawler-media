use api::Store;

#[test]
fn snapshot_retention_is_scoped_to_its_user_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .insert_search_snapshot("bob:only", "titles", "bob", "{}", 1)
        .unwrap();
    for index in 0..51 {
        store
            .insert_search_snapshot(
                &format!("alice:{index}"),
                "titles",
                "alice",
                "{}",
                index + 2,
            )
            .unwrap();
    }
    assert!(store.get_search_snapshot("bob:only").unwrap().is_some());
    assert!(store.get_search_snapshot("alice:0").unwrap().is_none());
    assert!(store.get_search_snapshot("alice:50").unwrap().is_some());
}
