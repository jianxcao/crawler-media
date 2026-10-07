use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn imported_ledger_row_can_restore_a_missing_library_file() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let download = tmp.path().join("matrix.mkv");
    std::fs::write(
        &download,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", download);
    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;

    let run = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/subscriptions/{}/run",
                subscribe["id"].as_str().unwrap()
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);

    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let row = &ledger["data"][0];
    let ledger_id = row["id"].as_str().unwrap();
    let source = row["source_path"]
        .as_str()
        .expect("completed download path is persisted");
    let destination = row["path"].as_str().unwrap();
    assert!(Path::new(source).is_file());

    std::fs::write(destination, b"keep the existing Library file").unwrap();
    let conflict = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/ledger/{ledger_id}/retransfer"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read(destination).unwrap(),
        b"keep the existing Library file"
    );

    std::fs::remove_file(destination).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = Path::new(destination).parent().unwrap().to_path_buf();
    store.verify_library_files(&[root], 123).unwrap();
    assert!(
        store
            .missing_at_by_path()
            .unwrap()
            .contains_key(destination)
    );
    drop(store);

    let restored = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/ledger/{ledger_id}/retransfer"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read(destination).unwrap(),
        std::fs::read(source).unwrap(),
        "retransfer recreates the missing Library file from the saved download"
    );
    assert!(
        !Store::open(tmp.path().join("data"))
            .unwrap()
            .missing_at_by_path()
            .unwrap()
            .contains_key(destination),
        "a successful retransfer clears the stale missing marker"
    );
}
