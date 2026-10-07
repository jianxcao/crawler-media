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
    store: Arc<parking_lot::Mutex<Store>>,
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
        // 生产路径会在已持有 Store 锁时再读 scrape 配置。try_lock 失败即证明嵌套锁。
        let guard = self
            .store
            .try_lock()
            .ok_or_else(|| "nested store.lock while fetching metadata".to_string())?;
        let _config = guard.get_scrape_config().ok();
        Ok(Some(media::ItemMeta {
            genre_ids: vec![16],
            origin_countries: vec!["JP".into()],
            ..media::ItemMeta::default()
        }))
    }
}

fn locking_preview_app(tmp: &tempfile::TempDir) -> axum::Router {
    let store = Arc::new(parking_lot::Mutex::new(
        Store::open(tmp.path().join("data")).unwrap(),
    ));
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
