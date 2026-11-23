use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use api::catalog::Catalog;
use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use indexer::ProfileSet;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::common::*;
use super::{
    StoreLockingCatalog, create_named_rule_set, locking_preview_state, seed_tv_routing_libraries,
};

struct SlowCacheWritingCatalog {
    store: Arc<Store>,
}

impl Catalog for SlowCacheWritingCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        std::thread::sleep(Duration::from_millis(650));
        let _ = self
            .store
            .put_catalog_cache("tmdb", "/tv/203483", "{}", 1, None);
        Ok(Some(Media {
            id: domain::MediaId::new(),
            kind,
            title: "两京十五日".into(),
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
        std::thread::sleep(Duration::from_millis(550));
        let _ = self
            .store
            .put_catalog_cache("tmdb", "/tv/203483/meta", "{}", 1, None);
        if tmdb_id == "203483" {
            return Ok(Some(media::ItemMeta {
                genre_ids: vec![18],
                origin_countries: vec!["CN".into()],
                ..media::ItemMeta::default()
            }));
        }
        Ok(None)
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn create_subscription_survives_slow_contended_catalog_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let catalog = Arc::new(SlowCacheWritingCatalog {
        store: store.clone(),
    });
    let state = ApiState::new_arc(
        store.clone(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap()
    .with_catalog(catalog);
    let app = router(state);
    let (_default_lib_id, china_lib_id, _default_filter, china_filter) =
        seed_tv_routing_libraries(&app, &tmp).await;

    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let cache_holder_store = store.clone();
    let cache_holder = std::thread::spawn(move || {
        cache_holder_store
            .with_writer(store::Db::Cache, |_| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv().unwrap();

    let slow_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    release_tx.send(()).unwrap();
    cache_holder.join().unwrap();
    assert_eq!(
        slow_res.status(),
        StatusCode::CREATED,
        "slow catalog + contended Db::Cache must not poison foreground write admission"
    );
    let slow_sub = json_data(slow_res).await;
    assert_eq!(slow_sub["library_id"], china_lib_id);
    assert_eq!(slow_sub["filter_id"], china_filter);
}

#[tokio::test]
async fn alias_only_subscription_inherits_stored_media_tmdb_for_match_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let state = locking_preview_state(&tmp);
    let store = state.store();
    let app = router(state);
    let (_default_lib_id, china_lib_id, _default_filter, _china_filter) =
        seed_tv_routing_libraries(&app, &tmp).await;

    store
        .insert_media(&Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Tv,
            title: "两京十五日 别名测试".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("203483".into()),
            douban_id: Some("36000001".into()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let alias_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": {
                    "kind": "tv",
                    "title": "两京十五日 别名测试",
                    "douban_id": "36000001"
                },
                "coverage": { "kind": "tv", "season": 2, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(alias_res.status(), StatusCode::CREATED);
    let alias_sub = json_data(alias_res).await;
    assert_eq!(
        alias_sub["library_id"], china_lib_id,
        "douban-only subscription input must inherit stored Media.tmdb_id for match_rules: {alias_sub}"
    );
}

#[tokio::test]
async fn stale_library_default_filter_falls_back_to_global_default_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let state = locking_preview_state(&tmp);
    let store = state.store();
    let app = router(state);

    let global_default = store
        .get_setting("default_filter_id")
        .unwrap()
        .expect("seeded global default filter");
    let transient_filter = create_named_rule_set(&app, "transient-lib-filter").await;
    let custom_root = tmp.path().join("tv-stale-filter");
    std::fs::create_dir_all(&custom_root).unwrap();
    let lib = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "失效规则库",
                    "kind": "tv",
                    "root_paths": [custom_root.display().to_string()],
                    "default_filter_id": transient_filter,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib_id = lib["id"].as_str().unwrap().to_string();

    store
        .delete_filter(transient_filter.parse().unwrap())
        .unwrap();

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "title_ref": "tmdb:tv:1399",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search",
                "library_id": lib_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let sub = json_data(res).await;
    assert_eq!(sub["filter_id"], global_default);
}

const CN_SEARCH_HTML: &str = r#"<!DOCTYPE html>
<html><body><table class="torrents">
  <tr class="torrent">
    <td><a class="name" title="两京十五日.S01E01.2026.2160p.WEB-DL.x265-GROUP" href="details.php?id=9">T</a></td>
    <td><a class="download" href="download.php?id=9&amp;passkey=abc"></a></td>
    <td class="size">2.00 GB</td><td class="seeders">20</td><td class="leechers">1</td>
    <td class="snatched">50</td><td class="upload_time">2026-01-15 08:30</td><td class="category">TV</td>
  </tr>
</table></body></html>"#;

fn cn_transfer_app(tmp: &tempfile::TempDir) -> axum::Router {
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let catalog = Arc::new(StoreLockingCatalog {
        store: store.clone(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("两京十五日.S01E01.2026.2160p.mkv");
    std::fs::write(&source, r#"{"resolution":"2160p","codec":"hevc"}"#).unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=9&passkey=abc", source);

    let state = ApiState::new_arc(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::from([("search", CN_SEARCH_HTML)]),
        }),
        downloader,
    )
    .unwrap()
    .with_catalog(catalog);
    router(state)
}

#[tokio::test]
async fn patch_null_library_id_on_cn_drama_transfers_to_default_library_without_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = cn_transfer_app(&tmp);
    create_site(&app).await;
    let (default_lib_id, china_lib_id, _default_filter, _china_filter) =
        seed_tv_routing_libraries(&app, &tmp).await;

    let created = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "title_ref": "tmdb:tv:203483",
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 },
                    "fetch_mode": "search"
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let sub_id = created["id"].as_str().unwrap();
    assert_eq!(created["library_id"], china_lib_id);

    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "library_id": Value::Null }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert!(json_data(patched).await["library_id"].is_null());

    let run_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{sub_id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        run_res.status(),
        StatusCode::OK,
        "transfer must route persisted library_id=None to default_library without fingerprint mismatch"
    );
    assert_eq!(json_data(run_res).await["ledger_rows"], 1);

    let ledger = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let rows = ledger.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["library_id"], default_lib_id);
    assert!(
        rows[0]["path"]
            .as_str()
            .unwrap()
            .starts_with(tmp.path().join("tv-default").to_str().unwrap()),
        "transferred path must land in tv-default, got {}",
        rows[0]["path"]
    );
}

struct SignalOnMetadataCatalog {
    inner: StoreLockingCatalog,
    matched_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl Catalog for SignalOnMetadataCatalog {
    fn search_movie(&self, q: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.inner.search_movie(q)
    }
    fn search_tv(&self, q: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.inner.search_tv(q)
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        self.inner.details(kind, tmdb_id)
    }
    fn metadata(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<media::ItemMeta>, String> {
        let res = self.inner.metadata(kind, tmdb_id);
        if let Some(tx) = self.matched_tx.lock().take() {
            let _ = tx.send(());
        }
        res
    }
}

#[tokio::test]
async fn concurrent_deletion_of_auto_matched_library_falls_back_to_default_library() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let (matched_tx, matched_rx) = std::sync::mpsc::channel();
    let catalog = Arc::new(SignalOnMetadataCatalog {
        inner: StoreLockingCatalog {
            store: store.clone(),
        },
        matched_tx: Mutex::new(Some(matched_tx)),
    });
    let state = ApiState::new_arc(
        store.clone(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap()
    .with_catalog(catalog);
    let app = router(state);
    let (_default_lib_id, china_lib_id, default_filter, _china_filter) =
        seed_tv_routing_libraries(&app, &tmp).await;

    let fence_store = store.clone();
    let remove_id = china_lib_id.clone();
    let racer = std::thread::spawn(move || {
        matched_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        fence_store.begin_library_removal(&remove_id).unwrap();
        fence_store
            .detach_subscribes_from_library(remove_id.parse().unwrap())
            .unwrap();
    });
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let sub_blocker_store = store.clone();
    let sub_blocker = std::thread::spawn(move || {
        sub_blocker_store
            .with_writer(store::Db::Subscribe, |_| {
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(120));
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv().unwrap();

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "title_ref": "tmdb:tv:203483",
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    sub_blocker.join().unwrap();
    racer.join().unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let sub = json_data(res).await;
    assert!(sub["library_id"].is_null());
    assert_eq!(sub["filter_id"], default_filter);
}
