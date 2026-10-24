//! Facet aggregation shares the bounded HTTP worker admission used by browse reads.
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::MediaKind;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

#[tokio::test]
async fn facets_obey_worker_budget_and_recover_after_capacity_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store.default_library(MediaKind::Movie).unwrap().unwrap().id;
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let state = ApiState::new(
        store,
        "secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap()
    .with_http_work(budget.clone());
    let app = router(state);
    let request = || {
        Request::builder()
            .uri(format!("/api/v1/libraries/{library}/facets?g=Drama"))
            .header("Authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap()
    };
    let reservation = budget.try_reserve().unwrap();
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(reservation);
    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"]["total"], 0);
    assert!(
        body["data"]["decades"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["count"] == 0)
    );
}
