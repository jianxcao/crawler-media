use std::collections::HashMap;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{Store, router};
use axum::http::StatusCode;
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use media::CatalogHit;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

struct RefreshCatalog {
    media: Media,
    requests: Mutex<Vec<String>>,
}

impl Catalog for RefreshCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        self.requests.lock().push(tmdb_id.to_string());
        if kind == self.media.kind && self.media.tmdb_id.as_deref() == Some(tmdb_id) {
            Ok(Some(self.media.clone()))
        } else {
            Ok(None)
        }
    }
}

fn refresh_catalog(tmdb_id: &str, title: &str) -> Arc<RefreshCatalog> {
    Arc::new(RefreshCatalog {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Movie,
            title: title.into(),
            year: Some(2003),
            original_title: Some(title.into()),
            aliases: Vec::new(),
            tmdb_id: Some(tmdb_id.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        requests: Mutex::new(Vec::new()),
    })
}

fn refresh_state_with(root: &std::path::Path, catalog: Arc<RefreshCatalog>) -> api::ApiState {
    state(
        root,
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .with_catalog(catalog)
}

fn refresh_state(root: &std::path::Path) -> api::ApiState {
    refresh_state_with(root, refresh_catalog("603", "The Matrix Reloaded"))
}

async fn post_filter_and_subscribe(app: &axum::Router) -> Value {
    let filter = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "preferred",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "The Matrix", "tmdb_id": "603" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    json_data(created).await
}

#[tokio::test]
async fn creating_a_subscribe_inserts_catalog_refresh_job_def() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(refresh_state(tmp.path()));
    post_filter_and_subscribe(&app).await;

    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/jobs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let defs = json_data(response).await;
    let defs = defs.as_array().unwrap();
    assert!(
        defs.iter().any(|row| row["kind"] == "catalog_refresh"),
        "{defs:?}"
    );
}

#[tokio::test]
async fn catalog_refresh_tick_upserts_media_title_and_year() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(refresh_state(tmp.path()));
    let created = post_filter_and_subscribe(&app).await;
    let media_id = created["media"]["id"].as_str().unwrap().to_string();

    let mut saw = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_data(response).await;
        if body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "catalog_refresh")
        {
            saw = true;
            break;
        }
    }
    assert!(saw, "CatalogRefresh Job did not run");

    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .get_media(media_id.parse().unwrap())
        .unwrap()
        .expect("Media");
    assert_eq!(media.title, "The Matrix Reloaded");
    assert_eq!(media.year, Some(2003));
    assert_eq!(media.original_title.as_deref(), Some("The Matrix Reloaded"));
    assert_eq!(media.tmdb_id.as_deref(), Some("603"));
}

/// 目录刷新必须只请求被选中的 refresh target：把 tmdb target 切到第二个别名后，出站
/// `details` 请求必须换成该别名，且被取消选择的别名仍能解析到同一 Media。
#[tokio::test]
async fn catalog_refresh_requests_only_the_selected_refresh_target() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = refresh_catalog("999", "The Matrix Reloaded II");
    let app = router(refresh_state_with(tmp.path(), catalog.clone()));
    let created = post_filter_and_subscribe(&app).await;
    let media_id: domain::MediaId = created["media"]["id"].as_str().unwrap().parse().unwrap();

    {
        let store = Store::open(tmp.path().join("data")).unwrap();
        store
            .add_media_alias(
                media_id,
                &domain::MediaAlias {
                    source: "tmdb".into(),
                    kind: MediaKind::Movie,
                    external_id: "999".into(),
                },
            )
            .unwrap();
    }
    {
        let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
        let flipped = db
            .execute(
                "UPDATE media_aliases SET is_refresh_target = CASE WHEN external_id = '999' THEN 1 ELSE 0 END
                 WHERE media_id = ?1 AND source = 'tmdb' AND kind = 'movie'",
                rusqlite::params![media_id.to_string()],
            )
            .unwrap();
        assert_eq!(
            flipped, 2,
            "expected the deselected and the selected tmdb alias"
        );
    }
    catalog.requests.lock().clear();

    let mut ran = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let body = json_data(
            app.clone()
                .oneshot(request(
                    "POST",
                    &format!("/api/v1/jobs/tick?now={now}"),
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap(),
        )
        .await;
        if body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "catalog_refresh")
        {
            ran = true;
            break;
        }
    }
    assert!(ran, "CatalogRefresh Job did not run");

    let requested = catalog.requests.lock().clone();
    assert_eq!(
        requested,
        vec!["999".to_string()],
        "the job must request only the selected refresh target"
    );

    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = store.get_media(media_id).unwrap().expect("Media");
    assert_eq!(media.title, "The Matrix Reloaded II");
    assert_eq!(
        store
            .media_refresh_alias(media_id, "tmdb")
            .unwrap()
            .unwrap()
            .external_id,
        "999"
    );
    let deselected = store
        .get_media_by_alias_kind("tmdb", "603", Some(MediaKind::Movie))
        .unwrap()
        .expect("deselected alias still resolves");
    assert_eq!(deselected.id, media_id);
}
