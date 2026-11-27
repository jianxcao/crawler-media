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

mod acl_and_preview;
mod create_and_runtime;

pub(super) struct StoreLockingCatalog {
    pub(super) store: Arc<Store>,
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
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        let title = match (kind, tmdb_id) {
            (MediaKind::Tv, "203483") => "两京十五日",
            (MediaKind::Tv, "1399") => "Game of Thrones",
            (MediaKind::Movie, "603") => "Spirited Away",
            _ => return Ok(None),
        };
        Ok(Some(Media {
            id: domain::MediaId::new(),
            kind,
            title: title.into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some(tmdb_id.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        }))
    }
    fn metadata(&self, _kind: MediaKind, tmdb_id: &str) -> Result<Option<media::ItemMeta>, String> {
        // Detect a writer inherited on this thread. Background writers on
        // other threads are legitimate and do not prevent catalog IO.
        store::WriterRankGuard::ensure_unheld(store::WriterRank::Library)
            .map_err(|error| format!("Writer inherited while fetching metadata: {error}"))?;
        let _config = self.store.get_scrape_config().ok();
        match tmdb_id {
            "603" => Ok(Some(media::ItemMeta {
                genre_ids: vec![16],
                origin_countries: vec!["JP".into()],
                ..media::ItemMeta::default()
            })),
            "203483" => Ok(Some(media::ItemMeta {
                genre_ids: vec![18],
                origin_countries: vec!["CN".into()],
                ..media::ItemMeta::default()
            })),
            "1399" => Ok(Some(media::ItemMeta {
                genre_ids: vec![18],
                origin_countries: vec!["US".into()],
                ..media::ItemMeta::default()
            })),
            _ => Ok(None),
        }
    }
}

pub(super) fn locking_preview_state(tmp: &tempfile::TempDir) -> api::ApiState {
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let catalog = Arc::new(StoreLockingCatalog {
        store: store.clone(),
    });
    api::ApiState::new_arc(
        store,
        "management-secret".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap()
    .with_catalog(catalog)
}

pub(super) fn locking_preview_app(tmp: &tempfile::TempDir) -> axum::Router {
    router(locking_preview_state(tmp))
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

pub(super) async fn create_named_rule_set(app: &axum::Router, name: &str) -> String {
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            json!({
                "name": name,
                "atoms": [{ "kind": "resolution", "value": "2160p", "priority": 100 }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    json_data(res).await["id"].as_str().unwrap().to_string()
}

pub(super) async fn seed_tv_routing_libraries(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> (String, String, String, String) {
    let default_filter = create_named_rule_set(app, "tv-default-1080p").await;
    let china_filter = create_named_rule_set(app, "china-4k").await;
    let tv_root = tmp.path().join("tv-default");
    let china_root = tmp.path().join("tv-china");
    std::fs::create_dir_all(&tv_root).unwrap();
    std::fs::create_dir_all(&china_root).unwrap();

    let default_lib = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "剧集库",
                    "kind": "tv",
                    "root_paths": [tv_root.display().to_string()],
                    "default_filter_id": default_filter,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let default_lib_id = default_lib["id"].as_str().unwrap().to_string();
    let set_default = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/libraries/{default_lib_id}/default"),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(set_default.status(), StatusCode::OK);
    let china_lib = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "国产剧",
                    "kind": "tv",
                    "root_paths": [china_root.display().to_string()],
                    "default_filter_id": china_filter,
                    "match_rules": [{
                        "field": "origin_countries",
                        "op": "any_of",
                        "values": ["CN"]
                    }],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    (
        default_lib_id,
        china_lib["id"].as_str().unwrap().to_string(),
        default_filter,
        china_filter,
    )
}

#[tokio::test]
async fn member_subscription_without_library_id_auto_routes_by_match_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let app = locking_preview_app(&tmp);
    let (_default_lib_id, china_lib_id, default_filter_id, china_filter_id) =
        seed_tv_routing_libraries(&app, &tmp).await;
    let (_member_id, member_token) = create_member(&app, "member-routing").await;

    let cn_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&member_token),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(cn_res.status(), StatusCode::CREATED);
    let cn_sub = json_data(cn_res).await;
    assert_eq!(
        cn_sub["library_id"], china_lib_id,
        "member subscription without library_id must auto-route CN drama to 国产剧: {cn_sub}"
    );
    assert_eq!(
        cn_sub["filter_id"], china_filter_id,
        "auto-routed subscription must inherit matched library default_filter_id: {cn_sub}"
    );

    let us_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&member_token),
            json!({
                "title_ref": "tmdb:tv:1399",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(us_res.status(), StatusCode::CREATED);
    let us_sub = json_data(us_res).await;
    assert!(
        us_sub["library_id"].is_null(),
        "non-matching subscription without library_id stays implicit (default library): {us_sub}"
    );
    assert_eq!(
        us_sub["filter_id"], default_filter_id,
        "implicit subscription should inherit default library default_filter_id: {us_sub}"
    );
}
