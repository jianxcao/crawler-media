use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tower::ServiceExt;

struct NoFetch;
impl indexer::Fetcher for NoFetch {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

fn state(root: &std::path::Path) -> crate::management::ApiState {
    crate::management::ApiState::new(
        crate::Store::open(root.join("data")).unwrap(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

fn check(method: &str) {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let before = state.store().transfer_settings().unwrap();
    let app = Router::new()
        .route("/", get(super::get).patch(super::patch))
        .with_state(state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = state.store();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        store
            .with_writer(store::Db::App, |_| {
                ready_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(3));
                Ok(())
            })
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (response, elapsed) = runtime.block_on(async {
        let body = if method == "PATCH" {
            format!(
                "{{\"expected_revision\":{},\"scrape\":false}}",
                before.revision
            )
        } else {
            String::new()
        };
        let request = Request::builder()
            .method(method)
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let start = Instant::now();
        let timer = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            start.elapsed()
        };
        tokio::join!(app.oneshot(request), timer)
    });
    let _ = release_tx.send(());
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(200),
        "executor stalled for {elapsed:?}"
    );
    let response = response.unwrap();
    if method == "PATCH" {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "1");
        let after = state.store().transfer_settings().unwrap();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
    }
}

#[test]
fn get_does_not_block_executor() {
    check("GET");
}
#[test]
fn patch_does_not_block_executor_and_busy_does_not_write() {
    check("PATCH");
}
