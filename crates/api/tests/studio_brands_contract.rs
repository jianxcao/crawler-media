use std::path::Path;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, EntityScope, LedgerId, Media, MediaId, MediaKind, MetadataAdoption,
    MetadataPreferences, QualitySource,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::{BrandMeta, CatalogHit, DoubanSubject, ItemMeta};
use serde_json::{Value, json};
use store::metadata::ScrapedDisplay;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

#[derive(Clone)]
struct BrandCatalog {
    tv_media: Media,
}

impl Catalog for BrandCatalog {
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
    fn details(&self, _kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        if tmdb_id == "40404" {
            return Err("TMDB HTTP 404".into());
        }
        Ok(Some(self.tv_media.clone()))
    }
    fn normalized_metadata(
        &self,
        _kind: MediaKind,
        _id: &str,
        scope: EntityScope,
        preferences: &MetadataPreferences,
    ) -> Result<Option<domain::NormalizedMetadata>, String> {
        let raw = r#"{
            "name": "CCTV Drama",
            "vote_average": 8.8,
            "vote_count": 200,
            "production_companies": [{"name": "New Classics Media"}]
        }"#;
        media::normalize_metadata("tmdb", scope, raw, preferences)
            .map(Some)
            .map_err(|e| e.to_string())
    }
    fn metadata(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<ItemMeta>, String> {
        Ok(Some(ItemMeta {
            overview: Some("Drama overview".into()),
            rating: Some("8.8".into()),
            vote_count: Some(200),
            release_date: Some("2024-05-01".into()),
            studios: vec![
                "CCTV-8".into(),
                "Tencent Video".into(),
                "New Classics Media".into(),
            ],
            studio_logo: Some("/2aXk02xBwG2Nhu4lftabfqSpwY3.png".into()),
            brands: vec![
                BrandMeta {
                    name: "CCTV-8".into(),
                    logo_path: Some("/2aXk02xBwG2Nhu4lftabfqSpwY3.png".into()),
                    kind: "network",
                },
                BrandMeta {
                    name: "Tencent Video".into(),
                    logo_path: Some("https://cdn.example.com/tencent.png".into()),
                    kind: "network",
                },
                BrandMeta {
                    name: "New Classics Media".into(),
                    logo_path: None,
                    kind: "studio",
                },
            ],
            ..Default::default()
        }))
    }
    fn douban_subject_with_credits(&self, _id: &str) -> Result<DoubanSubject, media::TmdbError> {
        Ok(DoubanSubject {
            subject: json!({
                "title": "豆瓣剧集",
                "type": "tv",
                "year": "2024",
                "rating": { "value": 8.4 }
            }),
            celebrities: json!({ "actors": [] }),
        })
    }
}

fn app_state(root: &Path, store: Store, tv_media: Media) -> ApiState {
    ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .unwrap()
    .with_catalog(Arc::new(BrandCatalog { tv_media }))
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

fn seed_tv_file(store: &Store, media_id: MediaId, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, b"video-bytes").unwrap();
    let spec = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id,
        path: path.to_string_lossy().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: Some("1080p".into()),
        codec: Some("H264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    publish_fixture_file(store, &spec).unwrap();
}

#[tokio::test]
async fn browse_cards_and_scraped_detail_expose_negated_tmdb_logos() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let scraped = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "CCTV Drama".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("9001".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&scraped).unwrap();

    let state = app_state(tmp.path(), store, scraped.clone());
    let tv_lib = state
        .store()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|lib| lib.kind == MediaKind::Tv)
        .expect("default tv library");
    let lib_id = tv_lib.id;
    let tv_root = &tv_lib.root_paths[0];
    seed_tv_file(
        &state.store(),
        scraped.id,
        &tv_root.join("CCTV Drama").join("S01E01.mkv"),
    );

    let app = router(state);
    let refresh = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/libraries/{lib_id}/items/{}/metadata/refresh",
                    scraped.id
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::OK);

    let list_body = get_json(&app, &format!("/api/v1/libraries/{lib_id}/items")).await;
    let items = list_body["data"].as_array().unwrap();
    let scraped_card = items
        .iter()
        .find(|row| row["media_item_id"] == scraped.id.to_string())
        .expect("scraped card");
    assert_eq!(scraped_card["studio"], "CCTV-8");
    assert_eq!(
        scraped_card["studio_logo"],
        "https://image.tmdb.org/t/p/w300/2aXk02xBwG2Nhu4lftabfqSpwY3.png"
    );

    let scraped_detail = get_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/items/{}", scraped.id),
    )
    .await;
    assert_eq!(
        scraped_detail["data"]["brands"],
        json!([
            {
                "name": "CCTV-8",
                "logo_url": "https://image.tmdb.org/t/p/w300/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
                "kind": "network"
            },
            {
                "name": "Tencent Video",
                "logo_url": "https://cdn.example.com/tencent.png",
                "kind": "network"
            },
            {
                "name": "New Classics Media",
                "logo_url": null,
                "kind": "studio"
            }
        ])
    );
}

#[tokio::test]
async fn item_detail_synthesizes_brands_for_legacy_rows_without_rescrape() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let legacy = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Legacy Show".into(),
        year: Some(2023),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("9002".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&legacy).unwrap();

    let state = app_state(tmp.path(), store, legacy.clone());
    let tv_lib = state
        .store()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|lib| lib.kind == MediaKind::Tv)
        .expect("default tv library");
    let lib_id = tv_lib.id;
    seed_tv_file(
        &state.store(),
        legacy.id,
        &tv_lib.root_paths[0].join("Legacy Show").join("S01E01.mkv"),
    );

    let pref = MetadataPreferences {
        language_priority: vec!["zh-CN".into()],
        cert_country_priority: vec!["CN".into()],
    };
    let normalized = media::normalize_metadata(
        "tmdb",
        EntityScope::Work,
        r#"{"name":"Legacy Show","production_companies":[{"name":"HBO"},{"name":"Warner Bros. Television"}]}"#,
        &pref,
    )
    .unwrap();
    let rev = state.store().media_revision(legacy.id).unwrap();
    state
        .store()
        .adopt_metadata(
            &MetadataAdoption {
                media_id: legacy.id,
                scope: EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "9002".into(),
                provider_work_id: "9002".into(),
                fetched_at: 1_000,
                normalized,
                expected_revision: rev,
                preferences: pref,
            },
            1_000,
        )
        .unwrap();
    state
        .store()
        .record_scraped_display(
            legacy.id,
            "zh-CN",
            ScrapedDisplay {
                rating: Some(9.1),
                release_date: Some("2023-01-15"),
                studio: Some("HBO"),
                studio_logo: Some("/hbo.png"),
                studio_logos: &[],
            },
        )
        .unwrap();

    let app = router(state);
    let legacy_detail = get_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/items/{}", legacy.id),
    )
    .await;
    assert_eq!(
        legacy_detail["data"]["brands"],
        json!([
            {
                "name": "HBO",
                "logo_url": "https://image.tmdb.org/t/p/w300/hbo.png",
                "kind": "network"
            },
            {
                "name": "Warner Bros. Television",
                "logo_url": null,
                "kind": "network"
            }
        ])
    );
}

#[tokio::test]
async fn unified_media_detail_and_douban_fallback_emit_info_brands() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "CCTV Drama".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("9001".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let app = router(app_state(tmp.path(), store, show));

    let tmdb_body = get_json(&app, "/api/v1/media/tv/9001").await;
    assert_eq!(
        tmdb_body["data"]["info"]["brands"],
        json!([
            {
                "name": "CCTV-8",
                "logo_url": "https://image.tmdb.org/t/p/w300/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
                "kind": "network"
            },
            {
                "name": "Tencent Video",
                "logo_url": "https://cdn.example.com/tencent.png",
                "kind": "network"
            },
            {
                "name": "New Classics Media",
                "logo_url": null,
                "kind": "studio"
            }
        ])
    );

    let empty_body = get_json(&app, "/api/v1/media/tv/40404").await;
    assert_eq!(empty_body["data"]["info"]["brands"], json!([]));

    let douban_body = get_json(&app, "/api/v1/media/douban/35000001").await;
    assert_eq!(douban_body["data"]["info"]["brands"], json!([]));
}
