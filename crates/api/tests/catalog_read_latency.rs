//! Complete catalog results must not monopolize Tokio workers or serialize a wall.
use api::{catalog::Catalog, ApiState, Store};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::{CatalogHit, CountryRow, PersonDetails};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const TOKEN: &str = "catalog-latency-reader";
struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
#[derive(Default)]
struct Source {
    active: AtomicUsize,
    peak: AtomicUsize,
    country_calls: AtomicUsize,
    language_calls: AtomicUsize,
}
impl Source {
    fn section(&self, label: &str) -> Result<Vec<CatalogHit>, String> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(40));
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(vec![hit(label)])
    }
}
impl Catalog for Source {
    fn source_name(&self) -> &'static str {
        "tmdb"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "tmdb").then_some(self)
    }
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        std::thread::sleep(Duration::from_millis(200));
        Ok(vec![hit("movie title")])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        let mut item = hit("TV title");
        item.media.kind = MediaKind::Tv;
        Ok(vec![item])
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.section("popular")
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.section("popular TV")
    }
    fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.section("top rated")
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.section("trending")
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.section("now playing")
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.section("upcoming")
    }
    fn filtered_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.section(query)
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn genres(&self, kind: MediaKind) -> Result<Vec<(i64, String)>, String> {
        Ok(vec![(
            18,
            if kind == MediaKind::Tv {
                "TV Drama"
            } else {
                "Movie Drama"
            }
            .into(),
        )])
    }
    fn configuration_countries(&self) -> Result<Vec<CountryRow>, String> {
        self.country_calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        Ok(vec![CountryRow {
            code: "CN".into(),
            english_name: "China".into(),
            native_name: "中国".into(),
        }])
    }
    fn configuration_languages(&self) -> Result<Vec<media::LanguageRow>, String> {
        self.language_calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        Ok(vec![media::LanguageRow {
            code: "zh".into(),
            english_name: "Chinese".into(),
            name: "中文".into(),
        }])
    }
    fn person_details(&self, _: &str) -> Result<Option<PersonDetails>, String> {
        Ok(Some(PersonDetails {
            name: "Jane Roe".into(),
            profile_path: Some("/jane.jpg".into()),
            items: vec![],
        }))
    }
}
fn hit(title: &str) -> CatalogHit {
    CatalogHit {
        media: Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: title.into(),
            original_title: None,
            year: Some(2026),
            tmdb_id: Some("603".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        },
        poster_path: Some("/poster.jpg".into()),
        backdrop_path: Some("/backdrop.jpg".into()),
        rating: Some(8.5),
        overview: Some("complete result".into()),
    }
}
fn fixture(source: Arc<dyn Catalog>) -> (tempfile::TempDir, axum::Router) {
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
async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
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
fn catalog_configuration_does_not_block_async_worker() {
    let (_dir, app) = fixture(Arc::new(Source::default()));
    let (response, tick) = runtime().block_on(async {
        let start = Instant::now();
        tokio::join!(get(app, "/api/v1/settings/countries"), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            start.elapsed()
        })
    });
    assert!(
        tick < Duration::from_millis(100),
        "a synchronous catalog read blocked the sole runtime worker for {tick:?}"
    );
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(
        response.1["data"]["countries"][0],
        serde_json::json!({"code":"CN","name":"中国","english_name":"China"})
    );
}
#[test]
fn catalog_title_search_preserves_results_without_blocking_async_worker() {
    let (_dir, app) = fixture(Arc::new(Source::default()));
    let (response, tick) = runtime().block_on(async {
        let start = Instant::now();
        tokio::join!(
            get(app, "/api/v1/search/titles?keyword=complete&provider=tmdb"),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                start.elapsed()
            }
        )
    });
    assert!(
        tick < Duration::from_millis(100),
        "search blocked the sole runtime worker for {tick:?}"
    );
    assert_eq!(response.0, StatusCode::OK);
    let titles = response.1["data"]["titles"].as_array().unwrap();
    assert!(titles.iter().any(|row| row["title"] == "movie title"));
    assert!(titles.iter().any(|row| row["title"] == "TV title"));
    assert!(titles
        .iter()
        .all(|row| row["poster_url"].as_str().is_some()));
}
#[test]
fn discover_sections_have_bounded_parallelism_and_complete_results() {
    let source = Arc::new(Source::default());
    let (_dir, app) = fixture(source.clone());
    let start = Instant::now();
    let (status, body) = runtime().block_on(get(app, "/api/v1/discover/movie?source=tmdb"));
    assert_eq!(status, StatusCode::OK);
    let sections = body["data"]["sections"].as_array().unwrap();
    assert_eq!(
        sections.len(),
        20,
        "no section may be omitted to meet latency"
    );
    assert!(sections.iter().all(|row| {
        row["items"]
            .as_array()
            .is_some_and(|items| items.len() == 1)
            && row.get("error").is_none()
    }));
    for id in [
        "featured-weekly",
        "trending-day",
        "now-playing",
        "upcoming",
        "popular",
        "documentary",
    ] {
        assert!(
            sections.iter().any(|row| row["id"] == id),
            "missing section {id}"
        );
    }
    assert!(
        start.elapsed() < Duration::from_millis(600),
        "independent section latency accumulated serially: {:?}",
        start.elapsed()
    );
    assert!(
        source.peak.load(Ordering::SeqCst) <= 4,
        "section execution must stay bounded"
    );
}
#[test]
fn cold_person_and_routing_results_keep_existing_fields() {
    let (_dir, app) = fixture(Arc::new(Source::default()));
    let (person, routing) = runtime().block_on(async {
        (
            get(app.clone(), "/api/v1/media/person/123").await,
            get(app, "/api/v1/libraries/routing-options").await,
        )
    });
    assert_eq!(person.0, StatusCode::OK);
    assert_eq!(person.1["data"]["name"], "Jane Roe");
    assert!(person.1["data"]["avatar_url"]
        .as_str()
        .is_some_and(|url| url.ends_with("/jane.jpg")));
    assert_eq!(routing.0, StatusCode::OK);
    assert_eq!(routing.1["data"]["movie_genres"][0]["label"], "Movie Drama");
    assert_eq!(routing.1["data"]["tv_genres"][0]["label"], "TV Drama");
    assert!(routing.1["data"]["region_presets"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty()));
}

struct FailedConfiguration;
impl Catalog for FailedConfiguration {
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
    fn configuration_languages(&self) -> Result<Vec<media::LanguageRow>, String> {
        Err("finite language source unavailable".into())
    }
    fn configuration_countries(&self) -> Result<Vec<CountryRow>, String> {
        Err("finite country source unavailable".into())
    }
}
#[test]
fn configuration_hit_reads_process_memory_without_another_catalog_call() {
    let source = Arc::new(Source::default());
    let (_dir, app) = fixture(source.clone());
    runtime().block_on(async {
        for (uri, counter, sample) in [
            (
                "/api/v1/settings/countries",
                &source.country_calls,
                serde_json::json!({"code":"CN","name":"中国","english_name":"China"}),
            ),
            (
                "/api/v1/settings/languages",
                &source.language_calls,
                serde_json::json!({"code":"zh","name":"中文","english_name":"Chinese"}),
            ),
        ] {
            let (status, body) = get(app.clone(), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
            let rows = if uri.ends_with("countries") {
                &body["data"]["countries"]
            } else {
                &body["data"]["languages"]
            };
            assert_eq!(rows[0], sample, "{uri}");
            assert_eq!(counter.load(Ordering::SeqCst), 1, "{uri}");
            let started = Instant::now();
            let (again_status, again) = get(app.clone(), uri).await;
            assert!(
                started.elapsed() < Duration::from_millis(50),
                "{uri} hit reread the catalog in {:?}",
                started.elapsed()
            );
            assert_eq!(again_status, StatusCode::OK, "{uri}: {again}");
            assert_eq!(again, body, "{uri}");
            assert_eq!(
                counter.load(Ordering::SeqCst),
                1,
                "{uri} hit called catalog"
            );
        }
    });
}

struct FlakyCountries {
    calls: AtomicUsize,
}
impl Catalog for FlakyCountries {
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
    fn configuration_countries(&self) -> Result<Vec<CountryRow>, String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Err("finite country source unavailable".into())
        } else {
            Ok(vec![CountryRow {
                code: "CN".into(),
                english_name: "China".into(),
                native_name: "中国".into(),
            }])
        }
    }
}
#[test]
fn configuration_failure_is_not_cached_and_the_next_success_is() {
    let source = Arc::new(FlakyCountries {
        calls: AtomicUsize::new(0),
    });
    let (_dir, app) = fixture(source.clone());
    runtime().block_on(async {
        let (status, body) = get(app.clone(), "/api/v1/settings/countries").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        let (status, body) = get(app.clone(), "/api/v1/settings/countries").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["countries"][0]["code"], "CN");
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
        let (status, _) = get(app, "/api/v1/settings/countries").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn catalog_configuration_failure_is_not_a_successful_empty_document() {
    let (_dir, app) = fixture(Arc::new(FailedConfiguration));
    runtime().block_on(async {
        for uri in ["/api/v1/settings/countries", "/api/v1/settings/languages"] {
            let (status, body) = get(app.clone(), uri).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{uri}: {body}");
            assert!(body.get("error").is_some(), "{body}");
        }
    });
}
#[test]
fn simultaneous_discover_walls_share_the_same_section_bound() {
    let source = Arc::new(Source::default());
    let (_dir, app) = fixture(source.clone());
    let (a, b) = runtime().block_on(async {
        tokio::join!(
            get(app.clone(), "/api/v1/discover/movie?source=tmdb"),
            get(app, "/api/v1/discover/movie?source=tmdb"),
        )
    });
    for (status, body) in [a, b] {
        assert_eq!(status, StatusCode::OK);
        let sections = body["data"]["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 20);
        assert!(sections.iter().all(|row| {
            row["items"]
                .as_array()
                .is_some_and(|items| items.len() == 1)
        }));
    }
    assert!(
        source.peak.load(Ordering::SeqCst) <= 4,
        "concurrent requests must share the fixed section capacity"
    );
}
