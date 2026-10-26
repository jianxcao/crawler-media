#[path = "management/common.rs"]
mod common;

use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn library_creation_honors_explicit_detection_switches() {
    let tmp = tempfile::tempdir().unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = api::router(state.clone());
    for enabled in [true, false] {
        let root = tmp.path().join(format!("tv-{enabled}"));
        std::fs::create_dir_all(&root).unwrap();
        let response = app
            .clone()
            .oneshot(common::request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({"name": format!("TV {enabled}"), "kind": "tv", "root_paths": [root],
                "detect_intros": enabled, "enable_fingerprint": enabled}),
            ))
            .await
            .unwrap();
        let data = common::json_data(response).await;
        assert_eq!(data["detect_intros"], enabled);
        assert_eq!(data["enable_fingerprint"], enabled);
        let library = state
            .store()
            .lock()
            .get_library(data["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(library.detect_intros, enabled);
        assert_eq!(library.enable_fingerprint, enabled);
    }
}
