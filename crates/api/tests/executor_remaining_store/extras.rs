use super::*;

#[test]
fn extras_failure_rolls_back_parked_identity_and_published_file_together() {
    let f = Fixture::new();
    let (file, path) = {
        let handle = f.state.store();
        let store = handle.lock();
        let row = store
            .file_coverage_for_media(f.media)
            .unwrap()
            .remove(0)
            .file;
        (row.file_id, row.path)
    };
    let conn = rusqlite::Connection::open(f._tmp.path().join("data/library.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_extras_delete BEFORE DELETE ON library_files
        BEGIN SELECT RAISE(ABORT, 'fixture rejects demotion delete'); END;",
    )
    .unwrap();
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request(
            "POST",
            "/api/v1/reidentify/extras",
            &f.token,
            json!({"file_ids":[file]}),
        )))
        .unwrap();
    let status = response.status();
    let body = runtime().block_on(json_response(response));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    let handle = f.state.store();
    let store = handle.lock();
    assert!(store.file_coverage_by_id(file).unwrap().is_some());
    assert!(
        store
            .get_unidentified(path.to_str().unwrap())
            .unwrap()
            .is_none(),
        "failed demotion cannot also park the published path"
    );
    assert!(path.is_file());
}
