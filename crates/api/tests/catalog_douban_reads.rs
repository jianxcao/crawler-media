//! Draft requires the new typed Catalog pair seam. Run compile RED first;
//! do not run the old raw-network route after adding only the trait method.
use api::{ApiState, Store, catalog::Catalog};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::{CatalogHit, DoubanSubject, TmdbError};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tower::ServiceExt;
const TOKEN: &str = "douban-wire-fixture";
struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline indexer".into()))
    }
}
struct Source;
impl Catalog for Source {
    fn source_name(&self) -> &'static str {
        "douban"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "douban").then_some(self)
    }
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
    fn details(&self, kind: MediaKind, id: &str) -> Result<Option<Media>, String> {
        Ok((id == "603").then(|| Media {
            id: domain::MediaId::new(),
            kind,
            title: "Local alias TMDB detail".into(),
            year: Some(1999),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some(id.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        }))
    }
    fn douban_subject_with_credits(&self, id: &str) -> Result<DoubanSubject, TmdbError> {
        assert_ne!(
            id, "local-alias",
            "adopted Douban alias must use its existing TMDB path"
        );
        match id {
            "missing" => Err(TmdbError::NotFound(id.into())),
            "unavailable" => Err(TmdbError::Http("finite source unavailable".into())),
            _ => {
                std::thread::sleep(Duration::from_millis(200));
                Ok(DoubanSubject {
                    subject: json!({
                        "id":id,"title":"Complete movie","type":"movie","year":"1994",
                        "intro":"Complete subject overview","cover_url":"https://example.invalid/poster.jpg",
                        "rating":{"value":9.5},"genres":["Drama"],"countries":["US"],
                        "languages":["English"],"release_date":"1994-07-06","aka":["Alias"],
                        "directors":[{"name":"Director"}],
                    }),
                    celebrities: json!({"actors":[{"id":"actor","name":"Actor","character":"Lead",
                        "avatar":{"large":"https://example.invalid/actor.jpg"}}],
                        "directors":[{"name":"Director"}]}),
                })
            }
        }
    }
}
fn fixture() -> (tempfile::TempDir, axum::Router) {
    fixture_with_source(Arc::new(Source))
}
fn fixture_with_source(source: Arc<dyn Catalog>) -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_catalog(source);
    (dir, api::router(state))
}
async fn get(app: axum::Router, id: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/media/douban/{id}"))
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn douban_wire_mapping_preserves_complete_subject_and_cast_without_blocking_async_worker() {
    let (_dir, app) = fixture();
    let (response, tick) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(get(app, "complete"), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            started.elapsed()
        })
    });
    assert!(
        tick < Duration::from_millis(100),
        "catalog blocked runtime for {tick:?}"
    );
    assert_eq!(response.0, StatusCode::OK);
    let data = &response.1["data"];
    assert_eq!(data["item"]["title"], "Complete movie");
    assert_eq!(data["item"]["year"], 1994);
    assert_eq!(
        data["item"]["poster_url"],
        "https://example.invalid/poster.jpg"
    );
    assert_eq!(data["info"]["overview"], "Complete subject overview");
    assert_eq!(data["info"]["cast"][0]["name"], "Actor");
    assert_eq!(data["info"]["cast"][0]["role"], "Lead");
    assert_eq!(
        data["info"]["cast"][0]["avatar_url"],
        "https://example.invalid/actor.jpg"
    );
    assert_eq!(data["info"]["directors"], json!(["Director"]));
    assert_eq!(data["info"]["country"], "US");
    assert_eq!(data["info"]["language"], "English");
    assert_eq!(data["info"]["released"], "1994-07-06");
    assert_eq!(data["info"]["aliases"], json!(["Alias"]));
    assert_eq!(data["posters"].as_array().unwrap().len(), 1);
}
#[test]
fn douban_missing_and_transient_errors_never_become_successful_empty_movies() {
    let (_dir, app) = fixture();
    runtime().block_on(async {
        for (id, expected) in [
            ("missing", StatusCode::NOT_FOUND),
            ("unavailable", StatusCode::BAD_GATEWAY),
        ] {
            let (status, body) = get(app.clone(), id).await;
            assert_eq!(status, expected, "{id}: {body}");
            assert!(body.get("error").is_some(), "{body}");
        }
    });
}

#[test]
fn douban_local_alias_keeps_tmdb_detail_fastpath() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    store
        .insert_media(&Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Movie,
            title: "Adopted local Movie".into(),
            year: Some(1999),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("603".into()),
            douban_id: Some("local-alias".into()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let state = ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(Source));
    let (status, body) = runtime().block_on(get(api::router(state), "local-alias"));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["item"]["tmdb_id"], "603");
    assert_eq!(body["data"]["item"]["title"], "Local alias TMDB detail");
    assert_eq!(body["data"]["item"]["year"], 1999);
}

struct UnrelatedTmdb;
impl Catalog for UnrelatedTmdb {
    fn source_name(&self) -> &'static str {
        "tmdb"
    }
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
    fn douban_subject_with_credits(&self, _: &str) -> Result<DoubanSubject, TmdbError> {
        panic!("Douban detail must resolve configured Douban source, not first source")
    }
}
#[test]
fn douban_detail_resolves_configured_fanout_source() {
    let source = api::catalog::FanoutCatalog::new(vec![Arc::new(UnrelatedTmdb), Arc::new(Source)]);
    let (_dir, app) = fixture_with_source(source);
    let (status, body) = runtime().block_on(get(app, "complete"));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["item"]["title"], "Complete movie");
    assert_eq!(body["data"]["info"]["cast"][0]["name"], "Actor");
}
