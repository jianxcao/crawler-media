//! MoviePilot-style TV episode 5-state panorama contract across Discover & Library detail.

use api::{ApiState, Store, catalog::Catalog};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{
    Coverage, EntityScope, FetchMode, FieldValue, FilterId, LibraryId, Media, MediaId, MediaKind,
    MetadataFields, QualityFact, SiteId, Subscribe, SubscribeFacts, SubscribeId, Torrent, User,
    UserId, UserRole,
};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "panorama-admin-token";
const MEMBER_TOKEN: &str = "panorama-member-token";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch(
            "network forbidden in panorama fixture".into(),
        ))
    }
}

struct PanoramaCatalog;

impl Catalog for PanoramaCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        if kind != MediaKind::Tv || tmdb_id != "9001" {
            return Ok(None);
        }
        Ok(Some(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "繁花".into(),
            year: Some(2023),
            original_title: Some("Blossoms Shanghai".into()),
            aliases: Vec::new(),
            tmdb_id: Some("9001".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        }))
    }
    fn tv_seasons(&self, tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        if tmdb_id != "9001" {
            return Ok(Vec::new());
        }
        Ok(vec![
            media::TvSeason {
                season_number: 1,
                name: "第 1 季".into(),
                episode_count: Some(6),
                air_date: Some("2023-12-27".into()),
                overview: None,
                poster_path: None,
            },
            media::TvSeason {
                season_number: 2,
                name: "第 2 季".into(),
                episode_count: Some(4),
                air_date: Some("2025-01-01".into()),
                overview: None,
                poster_path: None,
            },
        ])
    }
    fn cached_season_details(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Option<Vec<media::EpisodeMeta>>, String> {
        if tmdb_id != "9001" {
            return Ok(None);
        }
        let count = match season {
            1 => 6,
            2 => 4,
            _ => return Ok(None),
        };
        Ok(Some(
            (1..=count)
                .map(|ep| media::EpisodeMeta {
                    episode_number: ep,
                    name: Some(format!("S{season}E{ep}")),
                    overview: Some(format!("Overview {season}-{ep}")),
                    still_path: Some(format!("/still-s{season}e{ep}.jpg")),
                    ..Default::default()
                })
                .collect(),
        ))
    }
    fn cached_season_episodes(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
        if tmdb_id != "9001" {
            return Ok(None);
        }
        let dates: &[&str] = match season {
            1 => &[
                "2023-12-27",
                "2023-12-28",
                "2023-12-29",
                "2023-12-30",
                "2023-12-31",
                "2099-12-31",
            ],
            2 => &["2025-01-01", "2025-01-08", "2025-01-15", "2099-06-01"],
            _ => return Ok(None),
        };
        Ok(Some(
            dates
                .iter()
                .enumerate()
                .map(|(idx, date)| media::SeasonEpisode {
                    episode_number: (idx + 1) as u32,
                    air_date: Some((*date).into()),
                })
                .collect(),
        ))
    }
}

fn adopt_fields(store: &Store, media_id: MediaId, scope: EntityScope, fields: MetadataFields) {
    let rev = store.media_revision(media_id).unwrap().unwrap_or(0);
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id,
                scope,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "9001".into(),
                provider_work_id: "9001".into(),
                fetched_at: 1,
                normalized: domain::NormalizedMetadata {
                    fields,
                    field_origins: Default::default(),
                },
                expected_revision: Some(rev),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: Vec::new(),
                },
            },
            1,
        )
        .unwrap();
}

fn make_subscribe(user_id: UserId, media_id: MediaId, season: u32, lib: LibraryId) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id,
        media_id,
        coverage: Coverage::Tv {
            season,
            episode_from: 1,
            episode_to: None,
        },
        fetch_mode: FetchMode::Both,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(lib),
        tracking_state: "active".into(),
        follow_future: true,
        search_interval_secs: 1800,
    }
}

async fn get_json(app: Router, uri: &str, token: &str) -> Value {
    let req = Request::get(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
    serde_json::from_slice(&to_bytes(resp.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

fn adopt_fixture_seasons(store: &Store, media_id: MediaId) {
    adopt_fields(
        store,
        media_id,
        EntityScope::Work,
        MetadataFields {
            title: FieldValue::Value("繁花".into()),
            number_of_seasons: FieldValue::Value(2),
            ..Default::default()
        },
    );
    let s1_dates = [
        "2023-12-27",
        "2023-12-28",
        "2023-12-29",
        "2023-12-30",
        "2023-12-31",
        "2099-12-31",
    ];
    for (idx, date) in s1_dates.iter().enumerate() {
        let ep = (idx + 1) as u32;
        adopt_fields(
            store,
            media_id,
            EntityScope::Episode {
                season: 1,
                episode: ep,
            },
            MetadataFields {
                title: FieldValue::Value(format!("第 {ep} 集")),
                air_date: FieldValue::Value((*date).into()),
                ..Default::default()
            },
        );
    }
    let s2_dates = ["2025-01-01", "2025-01-08", "2025-01-15", "2099-06-01"];
    for (idx, date) in s2_dates.iter().enumerate() {
        let ep = (idx + 1) as u32;
        adopt_fields(
            store,
            media_id,
            EntityScope::Episode {
                season: 2,
                episode: ep,
            },
            MetadataFields {
                title: FieldValue::Value(format!("第二季第 {ep} 集")),
                air_date: FieldValue::Value((*date).into()),
                ..Default::default()
            },
        );
    }
}

fn seed_panorama_fixture(access_mode: &str) -> (tempfile::TempDir, ApiState, LibraryId, MediaId) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            "剧集库",
            &[root.to_str().unwrap()],
            access_mode,
            true,
            &[],
        )
        .unwrap();
    let lib_id: LibraryId = lib.id.parse().unwrap();

    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "繁花".into(),
        year: Some(2023),
        original_title: Some("Blossoms Shanghai".into()),
        aliases: Vec::new(),
        tmdb_id: Some("9001".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let ep1_path = root.join("Blossoms.S01E01.2160p.mkv");
    std::fs::write(&ep1_path, b"ep1-4k").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: ep1_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: Some("2160p".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let ep3_path = root.join("Blossoms.S01E03.1080p.mkv");
    std::fs::write(&ep3_path, b"ep3-missing").unwrap();
    let ep3_file = store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: ep3_path.display().to_string(),
            season: Some(1),
            episode: Some(3),
            resolution: Some("1080p".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let ep3_cov = store
        .file_coverage_by_id(ep3_file.file_id)
        .unwrap()
        .unwrap();
    store
        .apply_ledger_presence(
            &[store::LedgerPresenceObservation {
                path_key: ep3_cov.path_key.clone(),
                file: ep3_cov,
                missing_at: None,
                presence: domain::FilePresence::Missing,
                observed_source: None,
            }],
            store::LedgerPresenceMode::Verify,
            1_700_000_000,
        )
        .unwrap();

    adopt_fixture_seasons(&store, media.id);

    let state = ApiState::new(
        store,
        ADMIN_TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
    )
    .unwrap()
    .with_catalog(Arc::new(PanoramaCatalog));

    (dir, state, lib_id, media.id)
}

fn seed_subscriptions(store: &Store, admin_id: UserId, media_id: MediaId, lib_id: LibraryId) {
    let sub1 = make_subscribe(admin_id, media_id, 1, lib_id);
    store.insert_subscribe(&sub1).unwrap();
    store
        .record_pending_submission(
            sub1.id,
            100,
            &store::PendingDownload {
                torrent: Torrent {
                    site_id: SiteId::new(),
                    title: "Blossoms.Shanghai.S01E02.1080p.WEB-DL.H265.mkv".into(),
                    enclosure: "https://example.invalid/s01e02.torrent".into(),
                    size_bytes: Some(2_000_000_000),
                    seeders: Some(20),
                    free: true,
                    hr: Some(false),
                    imdb_id: None,
                    id: Some("t-102".into()),
                    leechers: Some(1),
                    snatched: Some(50),
                    upload_time: None,
                    detail_url: None,
                    category: None,
                    poster_url: None,
                    free_2x: false,
                    free_until: None,
                },
                release_override: None,
                downloader_id: None,
                submitted_at: Some(1_700_000_050),
            },
        )
        .unwrap();
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(4),
        QualityFact {
            score: 90,
            path: Some("/deleted/Blossoms.S01E04.1080p.mkv".into()),
        },
    );
    store.save_subscribe_facts(sub1.id, &facts).unwrap();

    let sub2 = make_subscribe(admin_id, media_id, 2, lib_id);
    store.insert_subscribe(&sub2).unwrap();
    store
        .record_wanted_grab(
            sub2.id,
            Some(2),
            None,
            "Blossoms.Shanghai.S02.Complete.2160p.WEB-DL",
            1_700_000_100,
        )
        .unwrap();
}

#[test]
fn discover_and_library_tv_panorama_reflect_all_five_episode_states() {
    let (_dir, state, lib_id, media_id) = seed_panorama_fixture("everyone");
    {
        let store = state.store();
        let admin_id = store.admin_user_id().unwrap();
        seed_subscriptions(&store, admin_id, media_id, lib_id);
    }
    let app = api::router(state);

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        // 1. Discover TV detail season summaries
        let tv = get_json(app.clone(), "/api/v1/media/tv/9001", ADMIN_TOKEN).await;
        let seasons = tv["data"]["seasons"].as_array().unwrap();
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0]["season_number"], 1);
        assert_eq!(seasons[0]["owned_episode_count"], 1);
        assert_eq!(seasons[0]["downloading_episode_count"], 1);
        assert_eq!(seasons[1]["season_number"], 2);
        assert_eq!(seasons[1]["owned_episode_count"], 0);
        assert_eq!(seasons[1]["downloading_episode_count"], 4);

        // 2. Discover Season 1 5-state episode breakdown
        let s1 = get_json(app.clone(), "/api/v1/media/tv/9001/season/1", ADMIN_TOKEN).await;
        let eps = s1["data"]["episodes"].as_array().unwrap();
        assert_eq!(eps.len(), 6);
        assert_eq!(eps[0]["status"], "in_library");
        assert_eq!(eps[0]["owned"], true);
        assert_eq!(eps[0]["library_id"], lib_id.to_string());
        assert_eq!(eps[0]["media_item_id"], media_id.to_string());
        assert_eq!(eps[0]["resolution"], "2160p");
        assert_eq!(eps[1]["status"], "downloading");
        assert_eq!(eps[1]["owned"], false);
        assert!(eps[1]["library_id"].is_null());
        assert_eq!(eps[2]["status"], "file_missing");
        assert_eq!(eps[3]["status"], "file_missing");
        assert_eq!(eps[4]["status"], "missing");
        assert_eq!(eps[5]["status"], "unaired");

        // 3. Library Item Detail preserves `seasons` and exposes `season_summaries` including unowned Season 2
        let detail = get_json(
            app.clone(),
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}"),
            ADMIN_TOKEN,
        )
        .await;
        assert_eq!(detail["data"]["seasons"], json!([1]));
        assert_eq!(
            detail["data"]["season_summaries"],
            json!([
                { "season_number": 1, "owned_count": 1, "total_count": 6, "downloading_count": 1 },
                { "season_number": 2, "owned_count": 0, "total_count": 4, "downloading_count": 4 },
            ])
        );

        // 4. Library Item Episodes for Season 1 and unowned Season 2
        let lib_s1 = get_json(
            app.clone(),
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/episodes?season=1"),
            ADMIN_TOKEN,
        )
        .await;
        let rows1 = lib_s1["data"].as_array().unwrap();
        let statuses1: Vec<&str> = rows1
            .iter()
            .map(|r| r["status"].as_str().unwrap())
            .collect();
        assert_eq!(
            statuses1,
            vec![
                "in_library",
                "downloading",
                "file_missing",
                "file_missing",
                "missing",
                "unaired",
            ]
        );
        assert_eq!(rows1[0]["resolution"], "2160p");
        assert!(rows1[1]["resolution"].is_null());

        let lib_s2 = get_json(
            app.clone(),
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/episodes?season=2"),
            ADMIN_TOKEN,
        )
        .await;
        let rows2 = lib_s2["data"].as_array().unwrap();
        assert_eq!(rows2.len(), 4);
        assert!(
            rows2
                .iter()
                .all(|r| r["status"] == "downloading" && r["owned"] == false)
        );
    });
}

#[test]
fn discover_season_panorama_respects_viewer_library_visibility() {
    let (_dir, state, lib_id, media_id) = seed_panorama_fixture("selected");
    {
        let store = state.store();
        let member = User {
            id: UserId::new(),
            login: "restricted-member".into(),
            enabled: true,
            role: UserRole::Member,
        };
        store.insert_user(&member).unwrap();
        store.set_user_token(member.id, MEMBER_TOKEN).unwrap();
    }
    let app = api::router(state);

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let admin_s1 = get_json(app.clone(), "/api/v1/media/tv/9001/season/1", ADMIN_TOKEN).await;
        let admin_eps = admin_s1["data"]["episodes"].as_array().unwrap();
        assert_eq!(admin_eps[0]["status"], "in_library");
        assert_eq!(admin_eps[0]["library_id"], lib_id.to_string());
        assert_eq!(admin_eps[0]["media_item_id"], media_id.to_string());

        let member_tv = get_json(app.clone(), "/api/v1/media/tv/9001", MEMBER_TOKEN).await;
        assert_eq!(member_tv["data"]["seasons"][0]["owned_episode_count"], 0);

        let member_s1 = get_json(app, "/api/v1/media/tv/9001/season/1", MEMBER_TOKEN).await;
        let member_eps = member_s1["data"]["episodes"].as_array().unwrap();
        assert_eq!(member_eps[0]["status"], "missing");
        assert_eq!(member_eps[0]["owned"], false);
        assert!(member_eps[0]["library_id"].is_null());
        assert!(member_eps[0]["media_item_id"].is_null());
        // Restricted library's missing ledger file (E3) must also stay hidden from non-member
        assert_eq!(member_eps[2]["status"], "missing");
    });
}

const OTHER_MEMBER_TOKEN: &str = "panorama-other-member-token";

fn seed_private_member_subscriptions(
    store: &Store,
    owner_id: UserId,
    media_id: MediaId,
    lib_id: LibraryId,
) {
    seed_subscriptions(store, owner_id, media_id, lib_id);
    let sub3 = make_subscribe(owner_id, media_id, 3, lib_id);
    store.insert_subscribe(&sub3).unwrap();
    store
        .record_wanted_grab(
            sub3.id,
            Some(3),
            Some(1),
            "Blossoms.Shanghai.S03E01.1080p.WEB-DL",
            1_700_000_200,
        )
        .unwrap();
}

#[test]
fn shared_library_panorama_hides_other_member_private_subscriptions() {
    let (_dir, state, lib_id, media_id) = seed_panorama_fixture("everyone");
    {
        let store = state.store();
        let member_a = User {
            id: UserId::new(),
            login: "member-a".into(),
            enabled: true,
            role: UserRole::Member,
        };
        let member_b = User {
            id: UserId::new(),
            login: "member-b".into(),
            enabled: true,
            role: UserRole::Member,
        };
        store.insert_user(&member_a).unwrap();
        store.set_user_token(member_a.id, MEMBER_TOKEN).unwrap();
        store.insert_user(&member_b).unwrap();
        store
            .set_user_token(member_b.id, OTHER_MEMBER_TOKEN)
            .unwrap();
        seed_private_member_subscriptions(&store, member_a.id, media_id, lib_id);
    }
    let app = api::router(state);

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let a_detail = get_json(
            app.clone(),
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}"),
            MEMBER_TOKEN,
        )
        .await;
        assert_eq!(
            a_detail["data"]["season_summaries"][0]["downloading_count"],
            1
        );
        assert_eq!(
            a_detail["data"]["season_summaries"][1]["downloading_count"],
            4
        );

        let b_detail = get_json(
            app.clone(),
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}"),
            OTHER_MEMBER_TOKEN,
        )
        .await;
        assert_eq!(
            b_detail["data"]["season_summaries"],
            json!([
                { "season_number": 1, "owned_count": 1, "total_count": 6, "downloading_count": 0 },
                { "season_number": 2, "owned_count": 0, "total_count": 4, "downloading_count": 0 },
            ]),
            "Member B must not see Member A's downloading counts or synthetic Season 3"
        );

        let b_s1 = get_json(
            app.clone(),
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/episodes?season=1"),
            OTHER_MEMBER_TOKEN,
        )
        .await;
        let b_statuses: Vec<&str> = b_s1["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["status"].as_str().unwrap())
            .collect();
        assert_eq!(
            b_statuses,
            vec![
                "in_library",
                "missing",
                "file_missing",
                "missing",
                "missing",
                "unaired"
            ],
            "Member B must not see Member A's downloading E2 or SubscribeFacts file_missing E4"
        );

        let b_s3 = get_json(
            app,
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/episodes?season=3"),
            OTHER_MEMBER_TOKEN,
        )
        .await;
        assert_eq!(
            b_s3["data"],
            json!([]),
            "Member B must not see Member A's synthetic Season 3 stubs"
        );
    });
}

#[test]
fn douban_and_person_detail_reject_unsafe_or_non_numeric_ids_with_400() {
    let (_dir, state, _, _) = seed_panorama_fixture("everyone");
    let app = api::router(state);

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for uri in [
            "/api/v1/media/douban/123%2e%2e%2fsecret",
            "/api/v1/media/douban/123%3ffoo%3dbar",
            "/api/v1/media/person/not-a-number",
            "/api/v1/media/person/999_extra",
        ] {
            let req = Request::get(uri)
                .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "expected 400 for {uri}"
            );
        }
    });
}
