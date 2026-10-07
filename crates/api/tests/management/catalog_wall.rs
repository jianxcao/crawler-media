use std::collections::HashMap;
use std::sync::Arc;

use api::catalog::Catalog;
use api::router;
use axum::http::StatusCode;
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use media::CatalogHit;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::catalog::{hit, test_catalog};
use super::common::*;

fn catalog_state(root: &std::path::Path) -> api::ApiState {
    state(
        root,
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .with_catalog(Arc::new(test_catalog()))
}
#[tokio::test]
async fn discover_movie_returns_popular_section() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let section = &body["data"]["sections"][0];
    assert_eq!(section["id"], "featured-weekly");
    assert_eq!(section["title"], "本周精选");
    assert_eq!(section["items"][0]["title"], "Trending Film");
    let ids: Vec<&str> = body["data"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    // Wall: 20 sections, first is featured-weekly (hero).
    assert_eq!(ids.len(), 20);
    assert_eq!(ids[0], "featured-weekly");
    assert!(ids.contains(&"popular"));
    assert!(ids.contains(&"top-rated"));
    assert!(ids.contains(&"now-playing"));
    assert!(ids.contains(&"trending-day"));
}

#[tokio::test]
async fn discover_tv_does_not_include_movie_titles() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let body = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/discover/tv",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let titles: Vec<String> = body["data"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|section| section["items"].as_array().unwrap())
        .map(|item| item["title"].as_str().unwrap().to_string())
        .collect();
    assert!(titles.contains(&"Breaking Bad".into()));
    assert!(!titles.iter().any(|t| t == "The Matrix"));
    let ids: Vec<&str> = body["data"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 20);
    assert!(!ids.contains(&"featured-weekly") || ids[0] == "featured-weekly");
    assert!(ids.contains(&"popular"));
    assert!(ids.contains(&"top-rated"));
    assert!(ids.contains(&"on-the-air"));
    assert!(ids.contains(&"trending-day"));
}

#[tokio::test]
async fn discover_rejects_unknown_source() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie?source=nope",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn discover_filtered_reports_unsupported_source() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie/filtered?sort=rating",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("不支持"),
        "{body}"
    );
}

struct UnsupportedCatalog;

impl Catalog for UnsupportedCatalog {
    fn source_name(&self) -> &'static str {
        "bangumi"
    }
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
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("该数据源不支持筛选发现".into())
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("该数据源不支持筛选发现".into())
    }
    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

struct EmptyTmdb;

impl Catalog for EmptyTmdb {
    fn source_name(&self) -> &'static str {
        "tmdb"
    }
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
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

struct FailingTmdb;

impl Catalog for FailingTmdb {
    fn source_name(&self) -> &'static str {
        "tmdb"
    }
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
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("tmdb upstream timeout".into())
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("tmdb upstream timeout".into())
    }
    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

#[tokio::test]
async fn discover_filtered_empty_tmdb_does_not_fail_because_bangumi_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let fanout =
        api::catalog::FanoutCatalog::new(vec![Arc::new(EmptyTmdb), Arc::new(UnsupportedCatalog)]);
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie/filtered?year=1800",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "empty TMDB must not become 502"
    );
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn discover_filtered_reports_502_when_supporting_source_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let fanout =
        api::catalog::FanoutCatalog::new(vec![Arc::new(FailingTmdb), Arc::new(UnsupportedCatalog)]);
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie/filtered?year=1800",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("timeout")
    );
}

#[tokio::test]
async fn discover_filtered_rejects_unknown_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/anime/filtered?sort=rating",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn image_proxy_rejects_unknown_host() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/images/proxy?url=https://evil.example/x.jpg",
            None,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn discover_rejects_unknown_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/anime",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn douban_tv_discover_sections_have_distinct_trending_and_popular() {
    struct FakeDoubanCatalog;
    impl Catalog for FakeDoubanCatalog {
        fn source_name(&self) -> &'static str {
            "douban"
        }
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
        fn trending_tv(&self) -> Result<Vec<CatalogHit>, String> {
            Ok(vec![hit("tv", "Realtime Trending Show", "111")])
        }
        fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
            Ok(vec![hit("movie", "Realtime Trending Movie", "888")])
        }
        fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
            Ok(vec![hit("movie", "Now Playing Movie", "999")])
        }
        fn tagged_movie(&self, tag: &str) -> Result<Vec<CatalogHit>, String> {
            if tag == "热门" {
                Ok(vec![hit("movie", "Tag Popular Movie", "777")])
            } else {
                Ok(vec![hit("movie", &format!("Tag {tag} Movie"), "666")])
            }
        }
        fn tagged_tv(&self, tag: &str) -> Result<Vec<CatalogHit>, String> {
            if tag == "热门" {
                Ok(vec![hit("tv", "Tag Popular Show", "222")])
            } else {
                Ok(vec![hit("tv", &format!("Tag {tag} Show"), "333")])
            }
        }
        fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
            Ok(None)
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let mut state = catalog_state(tmp.path());
    state = state.with_catalog(std::sync::Arc::new(FakeDoubanCatalog));
    let app = router(state);
    let body = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/discover/tv?source=douban",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;

    let sections = body["data"]["sections"].as_array().unwrap();
    let trending_section = sections
        .iter()
        .find(|s| s["id"] == "trending-day")
        .expect("trending-day section exists");
    let popular_section = sections
        .iter()
        .find(|s| s["id"] == "popular")
        .expect("popular section exists");

    let trending_title = trending_section["items"][0]["title"].as_str().unwrap();
    let popular_title = popular_section["items"][0]["title"].as_str().unwrap();

    assert_eq!(trending_title, "Realtime Trending Show");
    assert_eq!(popular_title, "Tag Popular Show");
    assert_ne!(trending_title, popular_title);
}

#[tokio::test]
async fn douban_movie_discover_sections_have_distinct_trending_now_playing_and_popular() {
    struct FakeDoubanCatalog;
    impl Catalog for FakeDoubanCatalog {
        fn source_name(&self) -> &'static str {
            "douban"
        }
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
        fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
            Ok(vec![hit("movie", "Realtime Trending Movie", "888")])
        }
        fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
            Ok(vec![hit("movie", "Now Playing Movie", "999")])
        }
        fn tagged_movie(&self, tag: &str) -> Result<Vec<CatalogHit>, String> {
            if tag == "热门" {
                Ok(vec![hit("movie", "Tag Popular Movie", "777")])
            } else {
                Ok(vec![hit("movie", &format!("Tag {tag} Movie"), "666")])
            }
        }
        fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
            Ok(None)
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let mut state = catalog_state(tmp.path());
    state = state.with_catalog(std::sync::Arc::new(FakeDoubanCatalog));
    let app = router(state);
    let body = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/discover/movie?source=douban",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;

    let sections = body["data"]["sections"].as_array().unwrap();
    let trending_section = sections
        .iter()
        .find(|s| s["id"] == "trending-day")
        .expect("trending-day section exists");
    let now_playing_section = sections
        .iter()
        .find(|s| s["id"] == "now-playing")
        .expect("now-playing section exists");
    let popular_section = sections
        .iter()
        .find(|s| s["id"] == "popular")
        .expect("popular section exists");

    let trending_title = trending_section["items"][0]["title"].as_str().unwrap();
    let now_playing_title = now_playing_section["items"][0]["title"].as_str().unwrap();
    let popular_title = popular_section["items"][0]["title"].as_str().unwrap();

    assert_eq!(trending_title, "Realtime Trending Movie");
    assert_eq!(now_playing_title, "Now Playing Movie");
    assert_eq!(popular_title, "Tag Popular Movie");
    assert_ne!(trending_title, popular_title);
    assert_ne!(now_playing_title, popular_title);
    assert_ne!(trending_title, now_playing_title);
}
