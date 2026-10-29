use store::Store;

#[test]
fn watch_report_failure_leaves_library_connection_in_autocommit() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let setup = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    setup.execute_batch("DROP TABLE playback_logs").unwrap();
    drop(setup);

    for _ in 0..2 {
        let error = store
            .playback_watch_report(None, None, 1_700_000_000, 30, 0)
            .unwrap_err();
        assert!(error.to_string().contains("playback_logs"), "{error}");
        assert!(
            !store.library_in_transaction(),
            "failed report must release its library snapshot"
        );
    }
}
