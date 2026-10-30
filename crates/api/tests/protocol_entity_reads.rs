//! Protocol entity reads select only the response page from published Library facts.
use api::{ApiState, Store, file_stat_read::FileStatRead};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

const TOKEN: &str = "protocol-entities";
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
#[derive(Default)]
struct Files {
    hydrated: AtomicUsize,
}
impl FileStatRead for Files {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn external_subtitles(&self, _: &Path) -> Result<Vec<library::SubtitleTrack>, String> {
        self.hydrated.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    files: Arc<Files>,
    movie: String,
    tv: String,
    root: PathBuf,
    series: MediaId,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("movies");
        let tv_root = dir.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&tv_root).unwrap();
        let movie = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id;
        let tv = store
            .create_library(
                MediaKind::Tv,
                "TV",
                &[tv_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id;
        let series = media(&store, MediaKind::Tv, "Featured series", None);
        let show = tv_root.join("Featured");
        std::fs::create_dir_all(&show).unwrap();
        std::fs::write(show.join("tvshow.nfo"), "<tvshow><title>Featured series</title><genre>Drama</genre><studio>Studio One</studio></tvshow>").unwrap();
        for episode in 1..=3 {
            publish(
                &store,
                series,
                &show.join(format!("S01E{episode:02}.mkv")),
                Some((1, episode)),
            );
        }
        let files = Arc::new(Files::default());
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap()
        .with_file_stat_read(files.clone());
        Self {
            _dir: dir,
            state,
            files,
            movie,
            tv,
            root,
            series,
        }
    }
    fn add_movie(&self, index: usize, title: &str, original: Option<&str>, genre: &str) -> MediaId {
        let store = self.state.store();
        let id = media(&store, MediaKind::Movie, title, original);
        let dir = self.root.join(format!("Movie{index}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("movie.nfo"),format!("<movie><title>{title}</title><originaltitle>{}</originaltitle><genre>{genre}</genre><studio>Studio One</studio><plot>overview-only-match</plot><actor><name>Jane Roe</name><role>Lead</role><tmdbid>42</tmdbid><thumb>https://example.invalid/jane.jpg</thumb></actor></movie>",original.unwrap_or(""))).unwrap();
        std::fs::write(dir.join("poster.jpg"), b"\xff\xd8\xffvisible").unwrap();
        publish(&store, id, &dir.join("movie.mkv"), None);
        id
    }
    fn reset(&self) {
        self.files.hydrated.store(0, Ordering::SeqCst);
    }
    fn hydrated(&self) -> usize {
        self.files.hydrated.load(Ordering::SeqCst)
    }
}
fn media(store: &Store, kind: MediaKind, title: &str, original: Option<&str>) -> MediaId {
    let value = Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: Some(2024),
        original_title: original.map(str::to_owned),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&value).unwrap();
    value.id
}
fn publish(store: &Store, id: MediaId, path: &Path, unit: Option<(u32, u32)>) {
    std::fs::write(path, b"video").unwrap();
    store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            media_id: id,
            path: path.display().to_string(),
            season: unit.map(|u| u.0),
            episode: unit.map(|u| u.1),
            ..Default::default()
        },
    )
    .unwrap();
}
async fn get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}
#[tokio::test]
async fn entity_lookup_work_is_bounded_by_result_page() {
    let fx = Fixture::new();
    for index in 0..20 {
        fx.add_movie(index, &format!("Featured {index:03}"), None, "Drama");
    }
    let app = api::router(fx.state.clone());
    let series = domain::TypedItemId::Work(fx.series).to_wire_string();
    let endpoints = [
        "/Genres?Limit=1".to_owned(),
        "/Studios?Limit=1".into(),
        "/Items/Counts".into(),
        "/Persons/Jane%20Roe".into(),
        "/Search/Hints?SearchTerm=Featured&Limit=1".into(),
        format!("/Shows/{series}/Seasons"),
    ];
    for uri in &endpoints {
        fx.reset();
        let value = get(&app, uri).await;
        assert!(
            fx.hydrated() <= 1,
            "{uri} hydrated {} rows for one result page: {value}",
            fx.hydrated()
        );
    }
    for index in 20..200 {
        fx.add_movie(index, &format!("Unrelated {index:03}"), None, "Drama");
    }
    for uri in &endpoints {
        fx.reset();
        get(&app, uri).await;
        assert!(
            fx.hydrated() <= 1,
            "{uri} work grew to {} hydrated rows",
            fx.hydrated()
        );
    }
}
#[tokio::test]
async fn protocol_counts_match_visible_paged_totals() {
    let fx = Fixture::new();
    let movie = fx.add_movie(0, "Featured movie", None, "Drama");
    publish(
        &fx.state.store(),
        movie,
        &fx.root.join("Movie0/alternate.mkv"),
        None,
    );
    let app = api::router(fx.state.clone());
    let counts = get(&app, "/Items/Counts").await;
    let movies = get(
        &app,
        &format!(
            "/Items?ParentId={}&IncludeItemTypes=Movie&Limit=50",
            fx.movie
        ),
    )
    .await;
    let series = get(
        &app,
        &format!("/Items?ParentId={}&IncludeItemTypes=Series&Limit=50", fx.tv),
    )
    .await;
    assert_eq!(counts["MovieCount"], movies["TotalRecordCount"]);
    assert_eq!(counts["SeriesCount"], series["TotalRecordCount"]);
    assert_eq!(counts["EpisodeCount"], 3);
    assert_eq!(counts["ItemCount"], 2);
}
#[tokio::test]
async fn search_hints_apply_visibility_and_limit() {
    let fx = Fixture::new();
    fx.add_movie(0, "Featured Alpha", None, "Drama");
    fx.add_movie(1, "Other", Some("Featured Original"), "Drama");
    fx.add_movie(2, "Excluded overview", None, "Drama");
    let app = api::router(fx.state.clone());
    let first = get(&app, "/Search/Hints?SearchTerm=Featured&Limit=1").await;
    let second = get(
        &app,
        "/Search/Hints?SearchTerm=Featured&StartIndex=1&Limit=1",
    )
    .await;
    assert_eq!(first["TotalRecordCount"], 3);
    assert_eq!(first["SearchHints"].as_array().unwrap().len(), 1);
    assert_eq!(second["TotalRecordCount"], 3);
    assert_ne!(
        first["SearchHints"][0]["Id"],
        second["SearchHints"][0]["Id"]
    );
    assert_eq!(
        get(&app, "/Search/Hints?SearchTerm=overview-only-match&Limit=1").await["TotalRecordCount"],
        0
    );
    fx.state.store().begin_library_removal(&fx.movie).unwrap();
    assert_eq!(
        get(&app, "/Search/Hints?SearchTerm=Featured&Limit=1").await["TotalRecordCount"],
        1
    );
}
#[tokio::test]
async fn seasons_are_series_scoped_and_preserve_play_state() {
    let fx = Fixture::new();
    let store = fx.state.store();
    let user = store.admin_user_id().unwrap();
    store
        .upsert_unit(
            user,
            fx.series,
            1,
            -1,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            false,
            10,
        )
        .unwrap();
    let app = api::router(fx.state.clone());
    let id = domain::TypedItemId::Work(fx.series).to_wire_string();
    fx.reset();
    let response = get(&app, &format!("/Shows/{id}/Seasons")).await;
    assert_eq!(response["TotalRecordCount"], 1);
    assert_eq!(response["Items"][0]["IndexNumber"], 1);
    assert_eq!(response["Items"][0]["UserData"]["Played"], true);
    assert_eq!(response["Items"][0]["UserData"]["UnplayedItemCount"], 0);
    assert_eq!(response["Items"][0]["UserData"]["PlayedPercentage"], 100.0);
    assert!(
        fx.hydrated() <= 1,
        "season summaries must not hydrate all episodes"
    );
}

#[tokio::test]
async fn entity_counts_and_representative_artwork_respect_library_visibility() {
    let fx = Fixture::new();
    let visible = fx.add_movie(0, "Visible", None, "Mystery Case");
    let hidden_root = fx._dir.path().join("hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    let store = fx.state.store();
    store
        .create_library(
            MediaKind::Movie,
            "Hidden",
            &[hidden_root.to_str().unwrap()],
            "members",
            false,
            &[],
        )
        .unwrap();
    let hidden = media(&store, MediaKind::Movie, "Hidden", None);
    std::fs::write(hidden_root.join("hidden.nfo"),"<movie><title>Hidden</title><genre>Mystery Case</genre><studio>Hidden Studio</studio></movie>").unwrap();
    std::fs::write(hidden_root.join("poster.jpg"), b"\xff\xd8\xffhidden").unwrap();
    publish(&store, hidden, &hidden_root.join("hidden.mkv"), None);
    let visible_file = store.file_coverage_for_media(visible).unwrap()[0]
        .file
        .file_id;
    let app = api::router(fx.state.clone());
    let rows = get(&app, "/Genres?SearchTerm=Mystery").await;
    assert_eq!(rows["TotalRecordCount"], 1);
    assert_eq!(rows["Items"][0]["Name"], "Mystery Case");
    assert_eq!(rows["Items"][0]["ChildCount"], 1);
    let wire = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"Mystery Case")
        .hyphenated()
        .to_string();
    assert_eq!(rows["Items"][0]["Id"], wire);
    let uri = "/Genres/Mystery%20Case/Images/Primary";
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    if response.status().is_redirection() {
        assert!(
            response.headers()["Location"]
                .to_str()
                .unwrap()
                .contains(&visible_file.to_string())
        );
    } else {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            &to_bytes(response.into_body(), usize::MAX).await.unwrap()[..],
            b"\xff\xd8\xffvisible"
        );
    }
    store.begin_library_removal(&fx.movie).unwrap();
    assert_eq!(
        get(&app, "/Genres?SearchTerm=Mystery").await["TotalRecordCount"],
        0
    );
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
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn representative_member(fx: &Fixture, suffix: u128, name: &str, poster: bool) -> MediaId {
    let store = fx.state.store();
    let id = MediaId::from_uuid(uuid::Uuid::from_u128(suffix));
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: name.into(),
            year: Some(2024),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
    let dir = fx.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("movie.nfo"),format!("<movie><title>{name}</title><genre>Representative</genre><studio>Representative</studio></movie>")).unwrap();
    if poster {
        std::fs::write(dir.join("poster.jpg"), b"\xff\xd8\xffrepresentative").unwrap();
    }
    publish(&store, id, &dir.join("movie.mkv"), None);
    id
}
#[tokio::test]
async fn entity_artwork_uses_a_visible_member_that_has_local_artwork() {
    let fx = Fixture::new();
    representative_member(&fx, 1, "First without poster", false);
    representative_member(&fx, 2, "Second with poster", true);
    let app = api::router(fx.state.clone());
    for uri in [
        "/Genres/Representative/Images/Primary",
        "/Studios/Representative/Images/Primary",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("Authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "{uri} must select the later member that has artwork, got {}",
            response.status()
        );
        if response.status().is_success() {
            assert_eq!(
                &to_bytes(response.into_body(), usize::MAX).await.unwrap()[..],
                b"\xff\xd8\xffrepresentative"
            );
        }
    }
}
