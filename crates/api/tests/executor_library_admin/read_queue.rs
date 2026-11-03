use super::*;

#[test]
fn maintenance_reads_wait_for_filesystem_capacity_then_succeed() {
    let f = Fixture::new();
    f.state
        .store()
        .insert_unidentified(f.file.path.display().to_string(), domain::Confidence::Low)
        .unwrap();
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
            if uri == "/api/v1/unidentified" {
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["data"][0]["total_size_bytes"], 7);
            }
        });
    }
}

#[test]
fn empty_unidentified_returns_while_filesystem_capacity_is_exhausted() {
    let f = Fixture::new();
    let _one = f.state.fs_work.try_reserve().unwrap();
    let _two = f.state.fs_work.try_reserve().unwrap();
    runtime().block_on(async {
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            router(f.state.clone()).oneshot(request("GET", "/api/v1/unidentified", &f.token)),
        )
        .await
        .expect("empty list should not wait for filesystem capacity")
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["data"], serde_json::json!([]));
    });
}
