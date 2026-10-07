//! Tests for filesystem directory browser endpoint.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

#[tokio::test]
async fn browse_fs_lists_child_directories_and_omits_files_and_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server_fs");
    std::fs::create_dir_all(root.join("movies")).unwrap();
    std::fs::create_dir_all(root.join("tv_shows")).unwrap();
    std::fs::create_dir_all(root.join(".hidden_dir")).unwrap();
    std::fs::write(root.join("some_file.txt"), "hello").unwrap();

    let app = app(&tmp);

    let res = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/fs/browse?path={}", root.display()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    let data = &body["data"];
    assert!(data["path"].as_str().unwrap().contains("server_fs"));

    let entries = data["entries"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        2,
        "only movies and tv_shows, no files or .hidden"
    );
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["movies", "tv_shows"]);
}
