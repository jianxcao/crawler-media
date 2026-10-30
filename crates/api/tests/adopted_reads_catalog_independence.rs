//! Adopted Library reads remain available while optional catalog origin is stalled.
use api::{ApiState, Store, catalog::Catalog};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use serde_json::Value;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "adopted-reader";
#[derive(Default)]
struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    fn stall(&self) {
        let guard = self.released.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(guard, Duration::from_secs(3), |v| !*v)
            .unwrap();
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.release();
    }
}
struct StalledCatalog(Arc<Gate>);
impl Catalog for StalledCatalog {
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
        self.0.stall();
        Err("origin unavailable".into())
    }
    fn metadata(&self, _: MediaKind, _: &str) -> Result<Option<media::ItemMeta>, String> {
        self.0.stall();
        Err("origin unavailable".into())
    }
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        self.0.stall();
        Err("origin unavailable".into())
    }
    fn similar(&self, _: MediaKind, _: &str) -> Result<Vec<CatalogHit>, String> {
        Err("upstream timeout".into())
    }
    fn season_episodes(&self, _: &str, _: u32) -> Result<Vec<media::SeasonEpisode>, String> {
        self.0.stall();
        Err("origin unavailable".into())
    }
}
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    library: String,
    media: MediaId,
    subscribe: domain::SubscribeId,
    gate: Arc<Gate>,
}
fn adopt(
    store: &Store,
    media: MediaId,
    scope: domain::EntityScope,
    fields: domain::MetadataFields,
) {
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media,
                scope,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "999999999".into(),
                provider_work_id: "999999999".into(),
                fetched_at: 1,
                normalized: domain::NormalizedMetadata {
                    fields,
                    field_origins: Default::default(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec![],
                },
            },
            1,
        )
        .unwrap();
}
fn fixture() -> Fixture {
    fixture_with_file(true)
}
fn fixture_with_file(owned: bool) -> Fixture {
    fixture_with_metadata(owned, true)
}
fn fixture_with_metadata(owned: bool, work_metadata: bool) -> Fixture {
    std::fs::create_dir_all("/tmp/crawler-storage-fixes").unwrap();
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("tv/Adopted");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Adopted",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id;
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Adopted series".into(),
        year: Some(2026),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("999999999".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    std::fs::write(root.join("poster.jpg"), b"local poster").unwrap();
    let path = root.join("episode.mkv");
    std::fs::write(&path, b"local episode").unwrap();
    if owned {
        store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    }
    if work_metadata {
        adopt(
            &store,
            media.id,
            domain::EntityScope::Work,
            domain::MetadataFields {
                overview: domain::FieldValue::Value("Adopted plot".into()),
                ..Default::default()
            },
        );
    }
    let today = store::now_rfc3339()[..10].to_string();
    adopt(
        &store,
        media.id,
        domain::EntityScope::Episode {
            season: 1,
            episode: 2,
        },
        domain::MetadataFields {
            air_date: domain::FieldValue::Value(today),
            ..Default::default()
        },
    );
    let gate = Arc::new(Gate::default());
    let state = ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(StalledCatalog(gate.clone())));
    let subscribe = insert_subscription(&state, media.id, owned.then_some(library.as_str()));
    Fixture {
        _dir: dir,
        state,
        library,
        media: media.id,
        subscribe,
        gate,
    }
}
fn insert_subscription(
    state: &ApiState,
    media: MediaId,
    library: Option<&str>,
) -> domain::SubscribeId {
    let store = state.store();
    let user = store.user_by_token(TOKEN).unwrap().unwrap().id;
    let filter = domain::Filter::new(domain::FilterId::new(), "empty", vec![]);
    store.insert_filter(&filter).unwrap();
    let subscribe = domain::SubscribeId::new();
    store
        .insert_subscribe(&domain::Subscribe {
            id: subscribe,
            user_id: user,
            media_id: media,
            coverage: domain::Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(3),
            },
            fetch_mode: domain::FetchMode::Search,
            filter_id: filter.id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: library.map(|library| library.parse().unwrap()),
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    if library.is_some() {
        let mut facts = subscribe::SubscribeFacts::default();
        facts.upsert(
            Some(1),
            Some(1),
            subscribe::QualityFact {
                score: 10,
                path: Some(
                    store.file_coverage_for_media(media).unwrap()[0]
                        .file
                        .path
                        .display()
                        .to_string(),
                ),
            },
        );
        store.save_subscribe_facts(subscribe, &facts).unwrap();
    }
    subscribe
}
async fn request(app: axum::Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("Content-Type", "application/json")
                .body(if method == "POST" {
                    Body::from("{}")
                } else {
                    Body::empty()
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopted_item_detail_and_playback_info_do_not_wait_for_catalog() {
    let fx = fixture();
    let app = api::router(fx.state.clone());
    let id = domain::TypedItemId::Unit {
        media: fx.media,
        key: domain::UnitKey::new(1, 1),
    }
    .to_wire_string();
    for uri in [
        format!("/UserFavoriteItems/{id}"),
        format!("/UserPlayedItems/{id}"),
    ] {
        assert_eq!(request(app.clone(), "POST", &uri).await.0, StatusCode::OK);
    }
    let reads = tokio::time::timeout(Duration::from_millis(500), async {
        let detail = request(app.clone(), "GET", &format!("/Items/{id}")).await;
        let playback = request(app.clone(), "POST", &format!("/Items/{id}/PlaybackInfo")).await;
        (detail, playback)
    })
    .await;
    fx.gate.release();
    let (detail, playback) =
        reads.expect("adopted detail/playback must not wait for optional catalog origin");
    assert_eq!(detail.0, StatusCode::OK);
    assert_eq!(detail.1["Overview"], "Adopted plot");
    assert_eq!(detail.1["UserData"]["IsFavorite"], true);
    assert_eq!(detail.1["UserData"]["Played"], true);
    assert_eq!(playback.0, StatusCode::OK);
    assert!(!playback.1["MediaSources"].as_array().unwrap().is_empty());
    fx.state.store().begin_library_removal(&fx.library).unwrap();
    assert_eq!(
        request(app, "GET", &format!("/Items/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_artwork_and_subscription_facts_survive_optional_catalog_miss() {
    let fx = fixture();
    let app = api::router(fx.state.clone());
    let reads = tokio::time::timeout(Duration::from_millis(500), async {
        let artwork = request(
            app.clone(),
            "GET",
            &format!(
                "/api/v1/libraries/{}/items/{}/artwork/candidates",
                fx.library, fx.media
            ),
        )
        .await;
        let subscription = request(
            app.clone(),
            "GET",
            &format!("/api/v1/subscriptions/{}", fx.subscribe),
        )
        .await;
        let arrivals = request(app.clone(), "GET", "/api/v1/subscriptions/today-arrivals").await;
        (artwork, subscription, arrivals)
    })
    .await;
    fx.gate.release();
    let (artwork, subscription, arrivals) =
        reads.expect("local artwork/subscription facts must not wait for origin");
    assert_eq!(artwork.0, StatusCode::OK);
    assert!(!artwork.1["data"]["posters"].as_array().unwrap().is_empty());
    assert_eq!(subscription.0, StatusCode::OK);
    let wanted = subscription.1["data"]["wanted"].as_array().unwrap();
    assert_eq!(
        wanted
            .iter()
            .find(|row| row["episode_number"] == 1)
            .unwrap()["status"],
        "imported"
    );
    assert_eq!(
        wanted
            .iter()
            .find(|row| row["episode_number"] == 2)
            .unwrap()["air_date"],
        store::now_rfc3339()[..10]
    );
    assert!(
        wanted
            .iter()
            .find(|row| row["episode_number"] == 3)
            .unwrap()["air_date"]
            .is_null()
    );
    assert_eq!(arrivals.0, StatusCode::OK);
    assert!(
        arrivals.1["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["status"] == "wanted" && row["episode"] == 2)
    );
}

#[tokio::test]
async fn similar_preserves_transient_catalog_errors() {
    let fx = fixture();
    fx.gate.release();
    let response = request(
        api::router(fx.state.clone()),
        "GET",
        &format!(
            "/api/v1/libraries/{}/items/{}/similar",
            fx.library, fx.media
        ),
    )
    .await;
    assert_eq!(
        response.0,
        StatusCode::BAD_GATEWAY,
        "a timeout is not a successful empty recommendation list"
    );
}

#[tokio::test]
async fn unbound_unowned_subscription_keeps_adopted_air_dates_without_origin() {
    let fx = fixture_with_file(false);
    fx.gate.release();
    assert!(
        fx.state
            .store()
            .file_coverage_for_media(fx.media)
            .unwrap()
            .is_empty()
    );
    let app = api::router(fx.state.clone());
    let subscription = request(
        app.clone(),
        "GET",
        &format!("/api/v1/subscriptions/{}", fx.subscribe),
    )
    .await;
    assert_eq!(subscription.0, StatusCode::OK);
    let wanted = subscription.1["data"]["wanted"].as_array().unwrap();
    assert_eq!(
        wanted
            .iter()
            .find(|row| row["episode_number"] == 2)
            .unwrap()["air_date"],
        store::now_rfc3339()[..10]
    );
    let arrivals = request(app, "GET", "/api/v1/subscriptions/today-arrivals").await;
    assert_eq!(arrivals.0, StatusCode::OK);
    assert!(
        arrivals.1["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["status"] == "wanted" && row["episode"] == 2)
    );
}

#[tokio::test]
async fn bound_subscription_uses_episode_only_adopted_schedule() {
    let fx = fixture_with_metadata(true, false);
    fx.gate.release();
    let result = request(
        api::router(fx.state.clone()),
        "GET",
        &format!("/api/v1/subscriptions/{}", fx.subscribe),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    let wanted = result.1["data"]["wanted"].as_array().unwrap();
    assert_eq!(
        wanted
            .iter()
            .find(|row| row["episode_number"] == 2)
            .unwrap()["air_date"],
        store::now_rfc3339()[..10]
    );
}
