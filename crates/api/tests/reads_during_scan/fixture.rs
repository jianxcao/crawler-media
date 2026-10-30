use api::password_login::PasswordLogin;
use api::{ApiState, Store};
use domain::{
    Coverage, FetchMode, FieldValue, Filter, FilterId, JobDefId, LibraryId, Media, MediaId,
    MediaKind, Subscribe, SubscribeId, User, UserId, UserRole,
};
use downloader::MemoryDownloader;
use indexer::ProfileSet;
use std::sync::Arc;

use super::doubles::{ImmediatePassword, OfflineCatalog, OfflineFetcher, PASSWORD};

pub const MEMBER_TOKEN: &str = "interactive-member-fixture-token";

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub state: ApiState,
    pub user: UserId,
    pub library: LibraryId,
    pub item: String,
    pub subscribe: SubscribeId,
    pub job: JobDefId,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let user = User {
            id: UserId::new(),
            login: "interactive-member".into(),
            enabled: true,
            role: UserRole::Member,
        };
        store.insert_user_with_password(&user, PASSWORD).unwrap();
        store.set_user_token(user.id, MEMBER_TOKEN).unwrap();
        let root = dir.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Interactive Library",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        let media = seed_media(&store, &root);
        let subscribe = seed_subscribe(&store, user.id, media, library);
        let state = ApiState::new(
            store,
            "interactive-admin-fixture-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(OfflineFetcher),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap()
        .with_catalog(Arc::new(OfflineCatalog))
        .with_password_login(Arc::new(PasswordLogin::new(Arc::new(ImmediatePassword), 2)));
        let job = state
            .jobs
            .lock()
            .ensure_def(jobs::NewDef {
                kind: jobs::JobKind::SubscribeSearch,
                name: "search interactive series".into(),
                enabled: true,
                schedule: None,
                payload: serde_json::json!({"subscribe_id": subscribe.to_string()}).to_string(),
                timeout_secs: None,
                concurrency_key: None,
            })
            .unwrap()
            .id;
        Self {
            _dir: dir,
            state,
            user: user.id,
            library,
            item: domain::TypedItemId::Work(media).to_wire_string(),
            subscribe,
            job,
        }
    }
}

fn seed_media(store: &Store, root: &std::path::Path) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Interactive series".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("episode.mkv");
    std::fs::write(&path, b"finite local interactive fixture").unwrap();
    store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media.id,
                scope: domain::EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "interactive-fixture".into(),
                provider_work_id: "interactive-fixture".into(),
                fetched_at: 1,
                normalized: domain::NormalizedMetadata {
                    fields: domain::MetadataFields {
                        overview: FieldValue::Value("adopted interactive plot".into()),
                        ..Default::default()
                    },
                    field_origins: Default::default(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: Vec::new(),
                },
            },
            1,
        )
        .unwrap();
    media.id
}

fn seed_subscribe(store: &Store, user: UserId, media: MediaId, library: LibraryId) -> SubscribeId {
    let filter = Filter::new(FilterId::new(), "Interactive filter", Vec::new());
    store.insert_filter(&filter).unwrap();
    let id = SubscribeId::new();
    store
        .insert_subscribe(&Subscribe {
            id,
            user_id: user,
            media_id: media,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(1),
            },
            fetch_mode: FetchMode::Search,
            filter_id: filter.id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: Some(library),
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    id
}
