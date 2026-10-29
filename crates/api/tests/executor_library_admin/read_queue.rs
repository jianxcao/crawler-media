use super::*;

#[test]
fn maintenance_reads_wait_for_filesystem_capacity_then_succeed() {
    let f = Fixture::new();
    let library = f.file.library_id;
    let media = f.file.media_id;
    for uri in [
        "/api/v1/unidentified".to_string(),
        "/api/v1/ledger".to_string(),
        "/api/v1/fs/browse?path=/tmp".to_string(),
        format!("/api/v1/libraries/{library}/duplicates"),
        format!("/api/v1/libraries/{library}/items/{media}/reidentification-preview"),
    ] {
        let one = f.state.fs_work.try_reserve().unwrap();
        let two = f.state.fs_work.try_reserve().unwrap();
        let app = router(f.state.clone());
        runtime().block_on(async {
            let mut read = tokio::spawn(app.oneshot(request("GET", &uri, &f.token)));
            let early = tokio::time::timeout(Duration::from_millis(100), &mut read).await;
            drop((one, two));
            assert!(early.is_err(), "{uri} must queue instead of returning 503");
            let response = tokio::time::timeout(Duration::from_secs(2), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        });
    }
}
