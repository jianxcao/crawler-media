use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Confidence, MediaKind};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

#[tokio::test]
async fn tv_unidentified_lists_and_filters_by_persisted_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let tv = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("Mystery.mkv");
    std::fs::write(&path, b"video").unwrap();
    store
        .insert_unidentified(path.display().to_string(), Confidence::Low)
        .unwrap();
    let expected_root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == tv.id.as_str())
        .unwrap()
        .id;
    let state = ApiState::new(
        store,
        "secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state);
    for url in [
        "/api/v1/unidentified".into(),
        format!("/api/v1/unidentified?library_id={}", tv.id),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(url)
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"][0]["library_id"], tv.id);
        assert_eq!(body["data"][0]["root_id"], expected_root);
    }
}

#[tokio::test]
async fn identify_and_transfer_may_change_parked_movie_owner_to_explicit_tv_target() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let path = tmp.path().join("Orbit.S01E01.mkv");
    std::fs::write(&path, b"video").unwrap();
    store
        .insert_unidentified(path.display().to_string(), Confidence::Low)
        .unwrap();
    let parked = store
        .get_unidentified(path.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        store.get_library(&parked.library_id).unwrap().unwrap().kind,
        MediaKind::Movie
    );
    let tv = store.default_library(MediaKind::Tv).unwrap().unwrap();
    let state = ApiState::new(
        store,
        "secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let shared = state.store();
    let response = router(state).oneshot(Request::builder().method("POST").uri("/api/v1/unidentified/claim")
        .header("authorization","Bearer secret").header("content-type","application/json")
        .body(Body::from(serde_json::json!({"path":path,"action":{"type":"identify_and_transfer","title":"Orbit","kind":"tv"}}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let store = shared.lock();
    let file = store
        .file_coverage_by_path(body["data"]["path"].as_str().unwrap())
        .unwrap()
        .unwrap()
        .file;
    assert_eq!(file.kind, MediaKind::Tv);
    assert_eq!(file.library_id.to_string(), tv.id);
    assert!(
        store
            .get_unidentified(path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
}
