use std::collections::HashMap;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{ScrapeStoreExt, Store, router};
use axum::http::StatusCode;
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

use super::common::*;

struct StoreLockingCatalog {
    store: Arc<Store>,
}

impl Catalog for StoreLockingCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn metadata(&self, _kind: MediaKind, tmdb_id: &str) -> Result<Option<media::ItemMeta>, String> {
        if tmdb_id != "603" {
            return Ok(None);
        }
        // Detect a writer inherited on this thread. Background writers on
        // other threads are legitimate and do not prevent catalog IO.
        store::WriterRankGuard::ensure_unheld(store::WriterRank::Library)
            .map_err(|error| format!("Writer inherited while fetching metadata: {error}"))?;
        let _config = self.store.get_scrape_config().ok();
        Ok(Some(media::ItemMeta {
            genre_ids: vec![16],
            origin_countries: vec!["JP".into()],
            ..media::ItemMeta::default()
        }))
    }
}

fn locking_preview_app(tmp: &tempfile::TempDir) -> axum::Router {
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let catalog = Arc::new(StoreLockingCatalog {
        store: store.clone(),
    });
    let state = api::ApiState::new_arc(
        store,
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap()
    .with_catalog(catalog);
    router(state)
}

#[tokio::test]
async fn tmdb_routing_preview_does_not_deadlock_on_store_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let anime_root = tmp.path().join("anime");
    std::fs::create_dir_all(&anime_root).unwrap();
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "动画",
                    "kind": "movie",
                    "root_paths": [anime_root.display().to_string()],
                    "match_rules": [{
                        "field": "genres",
                        "op": "any_of",
                        "values": [16]
                    }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let library_id = created["data"]["id"].as_str().unwrap().to_string();

    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/download-routing-preview",
            Some("management-secret"),
            json!({
                "kind": "movie",
                "tmdb_id": "603",
                "title": "Spirited Away",
                "year": 2001
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["data"]["library_id"], library_id,
        "metadata must run without holding Store so genre rules can match: {body}"
    );
    assert_eq!(body["data"]["route_matched"], true);
    assert!(
        body["data"]["route_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("动画"),
        "matched library should be named in the reason: {}",
        body["data"]["route_reason"]
    );
}

#[test]
fn catalog_metadata_accepts_an_independent_library_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let catalog = StoreLockingCatalog {
        store: store.clone(),
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer_store = store.clone();
    let writer = std::thread::spawn(move || {
        writer_store
            .with_writer(store::Db::Library, |_| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv().unwrap();
    let result = catalog.metadata(MediaKind::Movie, "603");
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    assert_eq!(result.unwrap().unwrap().genre_ids, vec![16]);
}

#[test]
fn catalog_metadata_rejects_an_inherited_library_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let catalog = StoreLockingCatalog {
        store: store.clone(),
    };
    store
        .with_writer(store::Db::Library, |_| {
            assert!(catalog.metadata(MediaKind::Movie, "603").is_err());
            Ok(())
        })
        .unwrap();
}
