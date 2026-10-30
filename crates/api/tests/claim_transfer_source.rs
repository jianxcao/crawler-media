//! A staged Transfer must keep the source generation it started with.
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use api::catalog::Catalog;
use api::{ApiState, Store, router};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use domain::{Media, MediaKind};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use serde_json::json;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}
struct PosterCatalog;
impl Catalog for PosterCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://fixture.invalid/poster.jpg".into()))
    }
}
struct ReplaceAfterCopy {
    source: PathBuf,
    entered: Arc<AtomicBool>,
}
impl api::PosterFetch for ReplaceAfterCopy {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        // This production seam is entered after stage_claim_file copied the
        // original source. Replace its pathname with a different file inode.
        assert_eq!(
            std::fs::read(&self.source).unwrap(),
            b"original source bytes"
        );
        let replacement = self.source.with_extension("replacement");
        std::fs::write(&replacement, b"replacement source bytes").unwrap();
        std::fs::rename(&replacement, &self.source).unwrap();
        self.entered.store(true, Ordering::SeqCst);
        Ok(b"poster fixture".to_vec())
    }
}

#[tokio::test]
async fn move_claim_rejects_a_source_replaced_after_copy_without_deleting_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("intake.mkv");
    std::fs::write(&source, b"original source bytes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_unidentified(source.display().to_string(), domain::Confidence::Low)
        .unwrap();
    store.set_transfer_mode("move").unwrap();
    store.set_scrape_enabled(true).unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let state = ApiState::new(
        store,
        "secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(PosterCatalog))
    .with_poster_fetch(Arc::new(ReplaceAfterCopy {
        source: source.clone(),
        entered: entered.clone(),
    }));
    let shared = state.store();
    let body = json!({"path":source,"action":{
        "type":"identify_and_transfer","title":"Claim","kind":"movie","year":2024,"tmdb_id":"1"
    }});
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/unidentified/claim")
                .header("Authorization", "Bearer secret")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        entered.load(Ordering::SeqCst),
        "the Copy/poster boundary was exercised"
    );
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&source).unwrap(), b"replacement source bytes");
    let store = shared.as_ref();
    assert!(
        store.list_file_coverage().unwrap().is_empty(),
        "stale destination must not publish"
    );
    assert!(
        store
            .get_unidentified(source.to_str().unwrap())
            .unwrap()
            .is_some()
    );
}
