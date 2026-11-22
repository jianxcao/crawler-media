use api::blocking_budget::BlockingBudget;
use api::media_server_provider::ApiServerProvider;
use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, EntityScope, FieldValue, LibraryId, Media, MediaId, MediaKind, MetadataAdoption,
    MetadataFields, MetadataGenre, MetadataPreferences, NormalizedMetadata, QualitySource,
    TypedItemId, UserId,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::{EntityKind, MediaServerProvider, QueryError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

const TOKEN: &str = "jellyfin-query-error-token";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    state: ApiState,
    user: UserId,
    library_id: LibraryId,
    root: PathBuf,
    series_id: MediaId,
    series_wire: String,
    folder_id: String,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();
        let root = tmp.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        let handle = state.store();
        let user = handle.admin_user_id().unwrap();
        let lib = handle
            .create_library(
                MediaKind::Tv,
                "Shows",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let library_id: LibraryId = lib.id.parse().unwrap();
        let (series_id, series_wire) = seed_tv_show(&handle, user, &root);
        let folder_id = media_server::provider::query::folder_id_for_root(&root);
        drop(handle);
        Self {
            _tmp: tmp,
            state,
            user,
            library_id,
            root,
            series_id,
            series_wire,
            folder_id,
        }
    }

    fn endpoints(&self) -> Vec<String> {
        vec![
            "/Genres".into(),
            "/Studios".into(),
            "/Items/Counts".into(),
            format!("/Shows/{}/Seasons", self.series_wire),
            "/Search/Hints?SearchTerm=test".into(),
            "/Users/Me/Items/Resume".into(),
            "/Shows/NextUp".into(),
        ]
    }
}

fn seed_tv_show(handle: &Store, user: UserId, root: &Path) -> (MediaId, String) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    handle.insert_media(&media).unwrap();
    let show_dir = root.join("Test Show");
    std::fs::create_dir_all(&show_dir).unwrap();
    let ep1 = show_dir.join("S01E01.mkv");
    let ep2 = show_dir.join("S01E02.mkv");
    std::fs::write(&ep1, b"ep1").unwrap();
    std::fs::write(&ep2, b"ep2").unwrap();
    std::fs::write(
        ep1.with_extension("nfo"),
        "<episodedetails><title>E1</title><genre>Drama</genre></episodedetails>",
    )
    .unwrap();
    for (ep, path) in [(1, &ep1), (2, &ep2)] {
        publish_fixture_file(
            handle,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(ep),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
    }
    handle
        .adopt_metadata(
            &MetadataAdoption {
                media_id: media.id,
                scope: EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "1".into(),
                provider_work_id: "1".into(),
                fetched_at: 1,
                normalized: NormalizedMetadata {
                    fields: MetadataFields {
                        genres: FieldValue::Value(vec![MetadataGenre {
                            source: "tmdb".into(),
                            source_id: None,
                            name: "Drama".into(),
                            filter_key: "drama".into(),
                        }]),
                        studios: FieldValue::Value(vec!["Studio Q".into()]),
                        ..Default::default()
                    },
                    field_origins: BTreeMap::new(),
                },
                expected_revision: Some(0),
                preferences: MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec![],
                },
            },
            1,
        )
        .unwrap();
    handle
        .upsert_unit(
            user,
            media.id,
            1,
            1,
            30_000,
            Some(false),
            None,
            Some(120_000),
            None,
            None,
            false,
            1_000,
        )
        .unwrap();
    (media.id, TypedItemId::Work(media.id).to_wire_string())
}

async fn assert_all_endpoints(app: &axum::Router, endpoints: &[String], expected: StatusCode) {
    for uri in endpoints {
        let resp = app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), expected, "GET {uri}");
    }
}

async fn assert_all_provider_unavailable(
    provider: &ApiServerProvider,
    user: UserId,
    series_id: MediaId,
    folder_id: &str,
) {
    assert!(matches!(
        provider
            .query_entities(user, EntityKind::Genre, None, None, 0, 20)
            .await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider
            .query_entities(user, EntityKind::Studio, None, None, 0, 20)
            .await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider.query_item_counts(user).await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider.query_seasons(user, series_id).await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider.search_hints_page(user, "test", 0, 20).await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider.next_up_items(user, None, 0, 20).await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider.resumable_items_page(user, None, 0, 20).await,
        Err(QueryError::Unavailable)
    ));
    assert!(matches!(
        provider
            .resumable_items_page(user, Some(folder_id), 0, 20)
            .await,
        Err(QueryError::Unavailable)
    ));
}

#[tokio::test]
async fn healthy_endpoints_succeed_and_query_unavailable_returns_503() {
    let fx = Fixture::new();
    let app = router(fx.state.clone());
    assert_all_endpoints(&app, &fx.endpoints(), StatusCode::OK).await;

    let _ = &fx.root;
    fx.state
        .store()
        .begin_library_integrity(fx.library_id, true, "err-test", 0)
        .unwrap();

    assert_all_endpoints(&app, &fx.endpoints(), StatusCode::SERVICE_UNAVAILABLE).await;
    let resume_folder = format!("/Users/Me/Items/Resume?ParentId={}", fx.folder_id);
    assert_all_endpoints(&app, &[resume_folder], StatusCode::SERVICE_UNAVAILABLE).await;

    let provider = ApiServerProvider::new(fx.state.clone());
    assert_all_provider_unavailable(&provider, fx.user, fx.series_id, &fx.folder_id).await;
}

#[tokio::test]
async fn saturated_http_work_budget_returns_503_across_all_endpoints_and_provider() {
    let mut fx = Fixture::new();
    fx.state.http_work = Arc::new(BlockingBudget::new(1));
    let _busy = fx.state.http_work.try_reserve().unwrap();

    let app = router(fx.state.clone());
    assert_all_endpoints(&app, &fx.endpoints(), StatusCode::SERVICE_UNAVAILABLE).await;
    let resume_folder = format!("/Users/Me/Items/Resume?ParentId={}", fx.folder_id);
    assert_all_endpoints(&app, &[resume_folder], StatusCode::SERVICE_UNAVAILABLE).await;

    let provider = ApiServerProvider::new(fx.state.clone());
    assert_all_provider_unavailable(&provider, fx.user, fx.series_id, &fx.folder_id).await;
}

#[tokio::test]
async fn closed_library_reader_pool_returns_503_across_all_endpoints_and_provider() {
    let fx = Fixture::new();
    fx.state
        .store()
        .reader_pool(api::store::Db::Library)
        .close();

    let app = router(fx.state.clone());
    assert_all_endpoints(&app, &fx.endpoints(), StatusCode::SERVICE_UNAVAILABLE).await;
    let resume_folder = format!("/Users/Me/Items/Resume?ParentId={}", fx.folder_id);
    assert_all_endpoints(&app, &[resume_folder], StatusCode::SERVICE_UNAVAILABLE).await;

    let provider = ApiServerProvider::new(fx.state.clone());
    assert_all_provider_unavailable(&provider, fx.user, fx.series_id, &fx.folder_id).await;
}
