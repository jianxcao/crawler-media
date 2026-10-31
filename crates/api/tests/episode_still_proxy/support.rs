use api::{ApiState, Store, catalog::Catalog};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{EntityScope, FieldValue, LibraryFile, Media, MediaId, MediaKind, MetadataFields};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "episode-still-fixture";
pub const REFERENCES: [(u32, &str, &str); 3] = [
    (
        4,
        "/relative-still.jpg",
        "https://image.tmdb.org/t/p/w300/relative-still.jpg",
    ),
    (
        5,
        "/t/p/original/original-still.jpg",
        "https://image.tmdb.org/t/p/original/original-still.jpg",
    ),
    (
        6,
        "https://image.tmdb.org/t/p/w300/absolute-still.jpg",
        "https://image.tmdb.org/t/p/w300/absolute-still.jpg",
    ),
];
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch(
            "network forbidden in still fixture".into(),
        ))
    }
}
pub struct StillCatalog {
    pub origin_fetches: AtomicUsize,
}
impl StillCatalog {
    pub fn new() -> Self {
        Self {
            origin_fetches: AtomicUsize::new(0),
        }
    }

    fn remember_origin(&self) -> Result<(), String> {
        self.origin_fetches.fetch_add(1, Ordering::SeqCst);
        Err("season hot path must not origin-fetch".into())
    }
}
impl Catalog for StillCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn season_details(&self, _: &str, _: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        self.remember_origin()?;
        Ok(Vec::new())
    }
    fn season_episodes(&self, _: &str, _: u32) -> Result<Vec<media::SeasonEpisode>, String> {
        self.remember_origin()?;
        Ok(Vec::new())
    }
    fn cached_season_details(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Option<Vec<media::EpisodeMeta>>, String> {
        Ok(Some(
            REFERENCES
                .iter()
                .map(|(number, path, _)| media::EpisodeMeta {
                    episode_number: *number,
                    name: Some(format!("Episode {number}")),
                    still_path: Some((*path).into()),
                    ..Default::default()
                })
                .collect(),
        ))
    }
    fn cached_season_episodes(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
        Ok(Some(
            REFERENCES
                .iter()
                .map(|(number, _, _)| media::SeasonEpisode {
                    episode_number: *number,
                    air_date: None,
                })
                .collect(),
        ))
    }
}
pub struct ColdSeason {
    pub origin_fetches: AtomicUsize,
}
impl Catalog for ColdSeason {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn season_details(&self, _: &str, _: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        self.origin_fetches.fetch_add(1, Ordering::SeqCst);
        Err("season hot path must not origin-fetch".into())
    }
    fn season_episodes(&self, _: &str, _: u32) -> Result<Vec<media::SeasonEpisode>, String> {
        self.origin_fetches.fetch_add(1, Ordering::SeqCst);
        Err("season hot path must not origin-fetch".into())
    }
    fn cached_season_details(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Option<Vec<media::EpisodeMeta>>, String> {
        Ok(None)
    }
    fn cached_season_episodes(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
        Ok(None)
    }
}
pub fn catalog_app(catalog: Arc<dyn Catalog>) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_catalog(catalog);
    (dir, api::router(state))
}
pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub state: ApiState,
    pub owned: LibraryFile,
    pub catalog: Arc<StillCatalog>,
}
impl Fixture {
    /// Construct before the runtime, so no background probe or real network starts.
    pub fn new() -> Self {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Still fixture",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Still series".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("123".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let path = root.join("Still.Series.S01E01.mkv");
        std::fs::write(&path, b"finite video fixture").unwrap();
        std::fs::write(
            path.with_file_name("Still.Series.S01E01-still.jpg"),
            b"local-still-bytes",
        )
        .unwrap();
        let owned = store::test_support::publish_fixture_file(
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
        adopt_fields(
            &store,
            media.id,
            EntityScope::Work,
            MetadataFields {
                title: FieldValue::Value("Series".into()),
                ..Default::default()
            },
        );
        for (episode, reference, _) in REFERENCES {
            adopt_fields(
                &store,
                media.id,
                EntityScope::Episode { season: 1, episode },
                MetadataFields {
                    title: FieldValue::Value(format!("Episode {episode}")),
                    still_url: FieldValue::Value(reference.into()),
                    ..Default::default()
                },
            );
        }
        let catalog = Arc::new(StillCatalog::new());
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap()
        .with_catalog(catalog.clone());
        Self {
            _dir: dir,
            state,
            owned,
            catalog,
        }
    }
    pub fn app(&self) -> Router {
        api::router(self.state.clone())
    }
}
fn adopt_fields(store: &Store, media_id: MediaId, scope: EntityScope, fields: MetadataFields) {
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id,
                scope,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "123".into(),
                provider_work_id: "123".into(),
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
pub async fn get(app: Router, uri: &str, authenticated: bool) -> axum::response::Response {
    let mut request = Request::get(uri);
    if authenticated {
        request = request.header("Authorization", format!("Bearer {TOKEN}"));
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
pub async fn get_json(app: Router, uri: &str) -> Value {
    let response = get(app, uri, true).await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}
pub fn assert_proxy_reference(reference: &str, expected: &str) {
    let url = url::Url::parse(&format!("http://fixture{reference}")).unwrap();
    assert_eq!(
        url.path(),
        "/images/proxy",
        "still must use the API image proxy: {reference}"
    );
    let target = url.query_pairs().find(|(key, _)| key == "url").unwrap().1;
    assert_eq!(target, expected);
}
