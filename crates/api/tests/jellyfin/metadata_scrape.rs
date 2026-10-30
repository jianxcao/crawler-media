use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct RichCatalog {
    show: Media,
    metadata: media::ItemMeta,
    origin_available: Arc<AtomicBool>,
}

impl api::catalog::Catalog for RichCatalog {
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
    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(Some(self.show.clone()))
    }
    fn metadata(&self, _kind: MediaKind, _id: &str) -> Result<Option<media::ItemMeta>, String> {
        if !self.origin_available.load(Ordering::SeqCst) {
            return Err("catalog origin offline; durable cache remains available".into());
        }
        Ok(Some(self.metadata.clone()))
    }
    fn cached_metadata_with_preferences(
        &self,
        _kind: MediaKind,
        _id: &str,
        _languages: &[String],
        _countries: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        Ok(Some(self.metadata.clone()))
    }
    fn metadata_with_preferences(
        &self,
        kind: MediaKind,
        id: &str,
        _languages: &[String],
        _countries: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        self.metadata(kind, id)
    }
    fn normalized_metadata(
        &self,
        _kind: MediaKind,
        _id: &str,
        scope: domain::EntityScope,
        preferences: &domain::MetadataPreferences,
    ) -> Result<Option<domain::NormalizedMetadata>, String> {
        if !self.origin_available.load(Ordering::SeqCst) {
            return Err("catalog origin offline; durable cache remains available".into());
        }
        let cast: Vec<_> = self
            .metadata
            .cast
            .iter()
            .map(|actor| {
                serde_json::json!({
                    "name": actor.name, "id": actor.person_id,
                    "profile_path": actor.avatar_path, "order": actor.order,
                    "roles": [{"character": actor.role, "episode_count": 16}],
                })
            })
            .collect();
        let body = serde_json::json!({
            "name": self.show.title, "original_name": self.show.original_title,
            "overview": self.metadata.overview, "first_air_date": self.metadata.release_date,
            "last_air_date": self.metadata.last_air_date,
            "vote_average": 8.5, "vote_count": self.metadata.vote_count,
            "original_language": self.metadata.original_language,
            "status": self.metadata.status, "tagline": self.metadata.tagline,
            "genres": [{"id": 16, "name": "Animation"}, {"id": 878, "name": "Science Fiction"}],
            "networks": [{"name": "Netflix"}], "origin_country": ["US"],
            "created_by": [{"name": "Craig Silverstein"}],
            "aggregate_credits": {"cast": cast},
            "content_ratings": {"results": [{"iso_3166_1": "US", "rating": "TV-14"}]},
        });
        media::normalize_metadata("tmdb", scope, &body.to_string(), preferences)
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn season_details(&self, _id: &str, _season: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        Ok(vec![media::EpisodeMeta {
            episode_number: 1,
            name: Some("Zero Day".into()),
            overview: Some("A new uploaded intelligence appears.".into()),
            still_path: Some("/still.jpg".into()),
            air_date: Some("2022-09-01".into()),
            runtime_minutes: Some(42),
            rating: Some("8.1".into()),
            vote_count: Some(81),
        }])
    }
    fn season_details_lang(
        &self,
        id: &str,
        season: u32,
        _language: &str,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        self.season_details(id, season)
    }
}

/// `tvshow_nfo` 必须在写文件之前落盘：Task 8/17 只在 publish_fixture_file 时观测旁车。
fn rich_tv_app(
    root: &Path,
    tvshow_nfo: &str,
    origin_available: Arc<AtomicBool>,
) -> (axum::Router, String, MediaId, PathBuf, PathBuf) {
    let store = Store::open(root.join("data")).unwrap();
    let library_root = root.join("library/tv");
    let show_root = library_root.join("Pantheon");
    std::fs::create_dir_all(&show_root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Pantheon".into(),
        year: Some(2022),
        original_title: Some("Pantheon".into()),
        aliases: Vec::new(),
        tmdb_id: Some("195339".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    let episode_path = show_root.join("Pantheon S01E01.mkv");
    std::fs::write(&episode_path, b"episode").unwrap();
    std::fs::write(show_root.join("tvshow.nfo"), tvshow_nfo).unwrap();
    let spec = store::test_support::FixtureFile {
        media_id: show.id,
        path: episode_path.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        transfer_mode: None,
        source_path: None,
        file_id: None,
    };
    store::test_support::publish_fixture_file(&store, &spec).unwrap();

    let metadata = media::ItemMeta {
        overview: Some("Uploaded intelligence changes humanity.".into()),
        rating: Some("8.5".into()),
        release_date: Some("2022-09-01".into()),
        last_air_date: Some("2025-02-21".into()),
        studios: vec!["Netflix".into()],
        content_rating: Some("TV-14".into()),
        original_language: Some("en".into()),
        status: Some("Ended".into()),
        vote_count: Some(1200),
        tagline: Some("What makes us human?".into()),
        number_of_seasons: Some(2),
        number_of_episodes: Some(16),
        directors: vec!["Craig Silverstein".into()],
        creators: vec!["Craig Silverstein".into()],
        genres: vec!["Animation".into(), "Science Fiction".into()],
        origin_countries: vec!["US".into()],
        cast: vec![
            media::CastRow {
                name: "Paul Dano".into(),
                role: Some("Caspian".into()),
                person_id: Some(6384),
                avatar_path: Some("/paul.jpg".into()),
                order: 0,
            },
            media::CastRow {
                name: "Rose McEwen".into(),
                role: Some("Maddie".into()),
                person_id: Some(12345),
                avatar_path: Some("/rose.jpg".into()),
                order: 1,
            },
            media::CastRow {
                name: "Catalog Only Actor".into(),
                role: Some("Navigator".into()),
                person_id: Some(777001),
                avatar_path: Some("/catalog-only.jpg".into()),
                order: 2,
            },
        ],
        ..Default::default()
    };
    let api_state = ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_image_fetch(Arc::new(|url| {
        Ok(("image/png".into(), url.as_bytes().to_vec()))
    }))
    .with_catalog(Arc::new(RichCatalog {
        show: show.clone(),
        metadata,
        origin_available,
    }));
    (
        router(api_state),
        library.id,
        show.id,
        show_root,
        episode_path,
    )
}

async fn json_get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn detail_fills_sparse_nfo_from_catalog_and_refresh_writes_rich_show_and_episode_nfo() {
    let tmp = tempfile::tempdir().unwrap();
    let origin_available = Arc::new(AtomicBool::new(false));
    let (app, library_id, media_id, show_root, episode_path) = rich_tv_app(
        tmp.path(),
        "<tvshow><title>Pantheon</title><actor><name>Legacy Actor</name><order>0</order></actor><actor><name>Paul Dano</name><role>Caspian</role><tmdbid>6384</tmdbid><order>1</order></actor><actor><name>Rose McEwen</name><role>Maddie</role><order>2</order></actor></tvshow>",
        origin_available.clone(),
    );

    let before_refresh = json_get(&app, &format!("/Items/{}", work_id(media_id))).await;
    assert_eq!(
        before_refresh["Overview"],
        "Uploaded intelligence changes humanity."
    );
    assert_eq!(before_refresh["PremiereDate"], "2022-09-01");
    assert_eq!(before_refresh["OfficialRating"], "TV-14");
    assert_eq!(before_refresh["Studios"][0]["Name"], "Netflix");
    assert_eq!(before_refresh["NumberOfSeasons"], 2);
    assert_eq!(before_refresh["NumberOfEpisodes"], 16);
    let people = before_refresh["People"].as_array().unwrap();
    assert_eq!(people.len(), 6);
    assert_eq!(people[0]["Name"], "Legacy Actor");
    assert_eq!(people[1]["Name"], "Paul Dano");
    assert_eq!(people[1]["PrimaryImageTag"], "profile");
    assert_eq!(people[1]["ProviderIds"]["Tmdb"], "6384");
    assert_eq!(people[2]["Name"], "Rose McEwen");
    assert_eq!(people[2]["PrimaryImageTag"], "profile");
    assert_eq!(people[2]["ProviderIds"]["Tmdb"], "12345");
    assert!(people.iter().any(|person| person["Type"] == "Director"));
    assert!(people.iter().any(|person| person["Type"] == "Writer"));

    // The complete detail above came from durable cached metadata while origin was
    // unavailable. Explicit refresh and the remaining catalog-focused routes may
    // now contact the source; their existing response/NFO assertions stay intact.
    origin_available.store(true, Ordering::SeqCst);

    let catalog_person = people
        .iter()
        .find(|person| person["Name"] == "Catalog Only Actor")
        .unwrap();
    assert_eq!(catalog_person["PrimaryImageTag"], "profile");
    let person_id = catalog_person["Id"].as_str().unwrap();

    // Person lookup/filtering use published adopted facts, not optional detail enrichment.
    let before_person = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/Persons/Catalog%20Only%20Actor")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(before_person.status(), StatusCode::NOT_FOUND);
    // R24: PersonIds reads local observed facts only; a person known solely to the live
    // catalog does not match (local hits are covered by
    // jellyfin_paged_metadata::audit::person_filter_is_local_and_survives_catalog_failure).
    let before_adoption = json_get(&app, &format!("/Items?PersonIds={person_id}")).await;
    assert_eq!(before_adoption["TotalRecordCount"], 0);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/libraries/{library_id}/items/{media_id}/metadata/refresh"
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_adopted_person_routes(&app, person_id).await;

    let show = library::read_nfo(&show_root.join("tvshow.nfo")).unwrap();
    assert_eq!(show.tagline.as_deref(), Some("What makes us human?"));
    assert_eq!(show.end_date.as_deref(), Some("2025-02-21"));
    assert_eq!(show.vote_count, Some(1200));
    assert_eq!(show.cast.len(), 3);
    assert!(
        show.cast
            .iter()
            .any(|person| person.name == "Catalog Only Actor")
    );
    let episode = library::read_nfo(&episode_path.with_extension("nfo")).unwrap();
    assert_eq!(episode.title.as_deref(), Some("Zero Day"));
    assert_eq!(episode.aired.as_deref(), Some("2022-09-01"));
    assert_eq!(episode.runtime_minutes.as_deref(), Some("42"));
}

async fn assert_adopted_person_routes(app: &axum::Router, person_id: &str) {
    let person_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/Persons/Catalog%20Only%20Actor")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(person_response.status(), StatusCode::OK);
    let person_details = json(person_response).await;
    assert_eq!(person_details["Id"], person_id);
    assert_eq!(person_details["ImageTags"]["Primary"], "profile");

    for uri in [
        "/Persons/Catalog%20Only%20Actor/Images/Primary".to_string(),
        format!("/Items/{person_id}/Images/Primary"),
    ] {
        let image = app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_delivered_person_image(image, "https://image.tmdb.org/t/p/w185/catalog-only.jpg")
            .await;
    }

    let person = json_get(app, "/Persons/Paul%20Dano").await;
    assert_eq!(person["ImageTags"]["Primary"], "profile");

    let person_image = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/Persons/Paul%20Dano/Images/Primary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_delivered_person_image(person_image, "https://image.tmdb.org/t/p/w185/paul.jpg").await;

    let rose_image = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/Persons/Rose%20McEwen/Images/Primary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_delivered_person_image(rose_image, "https://image.tmdb.org/t/p/w185/rose.jpg").await;
}
