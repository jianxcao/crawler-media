use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn catalog_cache_lists_seeded_rows_and_delete_removes_key() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    store
        .put_catalog_cache(
            "tmdb",
            "/search/movie?query=matrix",
            r#"{"results":[{"title":"The Matrix"}]}"#,
            100,
            Some(200),
        )
        .unwrap();
    drop(store);

    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/catalog/cache",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["source"], "tmdb");
    assert_eq!(row["cache_key"], "/search/movie?query=matrix");
    assert_eq!(row["fetched_at"], 100);
    assert_eq!(row["expires_at"], 200);
    assert_eq!(row["title"], "The Matrix");

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/catalog/cache?source=tmdb&cache_key=%2Fsearch%2Fmovie%3Fquery%3Dmatrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/catalog/cache",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert!(json_data(listed).await.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn catalog_cache_rejects_ambiguous_source_id_deletion() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    for key in ["/movie/1", "/movie/10"] {
        store
            .put_catalog_cache("tmdb", key, r#"{"title":"cached"}"#, 100, None)
            .unwrap();
    }
    drop(store);
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/catalog/cache?source=tmdb&source_id=1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = json_error(response).await;
    assert_eq!(error["code"], "catalog.invalid");
    let remaining = Store::open(&data).unwrap().list_catalog_cache().unwrap();
    assert_eq!(
        remaining.len(),
        2,
        "ambiguous source ids cannot remove other cache rows"
    );
}
