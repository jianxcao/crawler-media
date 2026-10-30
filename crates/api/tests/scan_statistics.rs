//! A completed scan refreshes planner statistics without reopening the process.
use api::{ApiState, ScrapeStoreExt, Store};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::{path::Path, sync::Arc, time::Duration};
use tower::ServiceExt;

struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch(
            "offline scan statistics fixture".into(),
        ))
    }
}

fn fixture(root: &Path) -> (ApiState, String) {
    let files = root.join("videos");
    std::fs::create_dir_all(&files).unwrap();
    for index in 0..240 {
        std::fs::write(
            files.join(format!("Video{index:03}.mkv")),
            b"finite local file",
        )
        .unwrap();
    }
    let store = Store::open(root.join("data")).unwrap();
    // Video scans also attach artwork. These finite ledger fixtures must not
    // launch ffmpeg once per deliberately invalid video while awaiting a scan.
    store
        .save_scrape_config(
            &serde_json::from_value(serde_json::json!({ "mirror_images": false })).unwrap(),
        )
        .unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Video,
            "Statistics",
            &[files.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
    let state = ApiState::new(
        store,
        "scan-statistics-fixture".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    (state, library.id)
}

fn analyzed_file_count(root: &Path) -> i64 {
    let connection = Connection::open(root.join("data/library.db")).unwrap();
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sqlite_stat1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return 0;
    }
    connection.query_row(
        "SELECT CAST(stat AS INTEGER) FROM sqlite_stat1 WHERE tbl='library_files' AND idx='files_library_file'",
        [], |row| row.get(0)).optional().unwrap().unwrap_or(0)
}

async fn request(app: axum::Router, method: &str, url: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(url)
                .header("Authorization", "Bearer scan-statistics-fixture")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn completed_scan_refreshes_library_statistics_without_restart() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    // Build outside Tokio so unrelated background probe drivers never start.
    let (state, library) = fixture(dir.path());
    assert!(analyzed_file_count(dir.path()) < 240);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let app = api::router(state.clone());
        let (status, body) = request(
            app.clone(),
            "POST",
            &format!("/api/v1/libraries/{library}/scan"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let operation = body["data"]["operation_id"].as_str().unwrap();
        let mut observed = Value::Null;
        let finished = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (status, body) = request(
                    app.clone(),
                    "GET",
                    &format!("/api/v1/libraries/{library}/scan-operations/{operation}"),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{body}");
                observed = body["data"].clone();
                match body["data"]["status"].as_str().unwrap() {
                    "completed" => {
                        assert_eq!(body["data"]["report"]["inserted"], 240);
                        break;
                    }
                    "failed" | "partial" | "cancelled" => {
                        panic!("scan did not complete: {observed}")
                    }
                    _ => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await;
        if finished.is_err() {
            let connection = Connection::open(dir.path().join("data/library.db")).unwrap();
            let registered: i64 = connection
                .query_row("SELECT count(*) FROM library_files", [], |row| row.get(0))
                .unwrap();
            panic!("scan timed out: registered={registered}, operation={observed}");
        }
    });
    assert!(
        analyzed_file_count(dir.path()) >= 240,
        "planner statistics must include the completed import while its Store remains open"
    );
}
