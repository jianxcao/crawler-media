use super::*;
use domain::{
    Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, SiteId, Subscribe, SubscribeFacts,
    SubscribeId, Torrent, UserId,
};

struct StubSeason2Catalog;
impl crate::catalog::Catalog for StubSeason2Catalog {
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
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn cached_season_episodes(
        &self,
        _: &str,
        season: u32,
    ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
        if season != 2 {
            return Ok(None);
        }
        Ok(Some(vec![media::SeasonEpisode {
            episode_number: 3,
            air_date: Some("2026-01-15".into()),
        }]))
    }
}

#[test]
fn season_inferred_pending_release_uses_subscription_season_in_today_arrivals() {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "葬送的芙莉莲".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("209867".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Tv {
            season: 2,
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
        library_id: None,
        tracking_state: "active".into(),
        follow_future: true,
        search_interval_secs: 1800,
    };
    let raw_title = "Sousou.no.Frieren.EP03.1080p.WEB-DL.mkv";
    let check_parsed = release::parse(raw_title);
    assert_eq!(check_parsed.season, Some(1));
    assert_eq!(check_parsed.episode, Some(3));
    assert!(check_parsed.season_inferred);
    let pending = vec![(
        1,
        crate::store::PendingDownload {
            torrent: Torrent {
                site_id: SiteId::new(),
                title: raw_title.into(),
                enclosure: "https://example.invalid/03.torrent".into(),
                size_bytes: None,
                seeders: None,
                free: false,
                hr: None,
                imdb_id: None,
                id: None,
                leechers: None,
                snatched: None,
                upload_time: None,
                detail_url: None,
                category: None,
                poster_url: None,
            },
            release_override: None,
            downloader_id: None,
            submitted_at: Some(1_768_435_200),
        },
    )];
    let now = parse_upload_time("2026-01-15").unwrap();
    let items = build_arrivals(
        vec![ArrivalSnapshot {
            subscribe,
            media,
            pending,
            facts: SubscribeFacts::default(),
            rows: Vec::new(),
            episodes: Vec::new(),
        }],
        &StubSeason2Catalog,
        now,
    );
    assert_eq!(
        items.len(),
        1,
        "grabbed S02E03 must suppress duplicate wanted item: {items:?}"
    );
    assert_eq!(items[0]["status"], "grabbed");
    assert_eq!(items[0]["season"], 2);
    assert_eq!(items[0]["episode"], 3);
}

fn seed_arrivals_store(tmp: &tempfile::TempDir) -> (crate::Store, UserId, UserId, FilterId, Media) {
    let store = crate::Store::open(tmp.path().join("data")).unwrap();
    let admin_id = UserId::new();
    let member_id = UserId::new();
    for (id, login, role) in [
        (admin_id, "admin", domain::UserRole::Admin),
        (member_id, "member", domain::UserRole::Member),
    ] {
        store
            .insert_user_with_password(
                &domain::User {
                    id,
                    login: login.into(),
                    enabled: true,
                    role,
                },
                "secret",
            )
            .unwrap();
    }
    let filter = domain::Filter::new(FilterId::new(), "default", Vec::new());
    store.insert_filter(&filter).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "葬送的芙莉莲".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("209867".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    (store, admin_id, member_id, filter.id, media)
}

fn publish_s2e3(store: &crate::Store, media_id: MediaId, dir: &std::path::Path) {
    let path = dir.join("S02E03.mkv");
    std::fs::write(&path, b"video").unwrap();
    store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id,
            path: path.display().to_string(),
            season: Some(2),
            episode: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
}

fn insert_s2_subscribe(
    store: &crate::Store,
    user_id: UserId,
    media_id: MediaId,
    filter_id: FilterId,
    library_id: Option<domain::LibraryId>,
) {
    store
        .insert_subscribe(&Subscribe {
            id: SubscribeId::new(),
            user_id,
            media_id,
            coverage: Coverage::Tv {
                season: 2,
                episode_from: 1,
                episode_to: None,
            },
            fetch_mode: FetchMode::Search,
            filter_id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id,
            tracking_state: "active".into(),
            follow_future: true,
            search_interval_secs: 1800,
        })
        .unwrap();
}

#[test]
fn untargeted_subscription_ignores_non_default_and_hidden_default_libraries() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, admin_id, _, filter_id, media) = seed_arrivals_store(&tmp);
    let default_root = tmp.path().join("tv-default");
    let other_root = tmp.path().join("tv-other");
    std::fs::create_dir_all(&default_root).unwrap();
    std::fs::create_dir_all(&other_root).unwrap();

    let default_lib = store
        .create_library(
            MediaKind::Tv,
            "默认剧集",
            &[default_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.set_default_library(&default_lib.id).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "其他剧集",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    insert_s2_subscribe(&store, admin_id, media.id, filter_id, None);

    publish_s2e3(&store, media.id, &other_root);

    let snaps = load_arrivals(&store, admin_id).unwrap();
    assert!(
        snaps[0].rows.is_empty(),
        "None subscription must not inherit coverage from non-default library"
    );

    publish_s2e3(&store, media.id, &default_root);
    let snaps_default = load_arrivals(&store, admin_id).unwrap();
    assert_eq!(snaps_default[0].rows.len(), 1);
    assert_eq!(
        snaps_default[0].rows[0].file.library_id.to_string(),
        default_lib.id
    );

    store
        .update_library(&default_lib.id, None, None, Some(("everyone", false, &[])))
        .unwrap();
    let snaps_hidden = load_arrivals(&store, admin_id).unwrap();
    assert!(
        snaps_hidden[0].rows.is_empty(),
        "None subscription must not auto-route to admin_visible=false default library"
    );
}

#[test]
fn explicit_subscription_sees_admin_hidden_library_but_enforces_acl_and_accepts_work() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, admin_id, member_id, filter_id, media) = seed_arrivals_store(&tmp);
    let hidden_root = tmp.path().join("tv-hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    let hidden_lib = store
        .create_library(
            MediaKind::Tv,
            "隐藏专库",
            &[hidden_root.to_str().unwrap()],
            "selected",
            false,
            &[],
        )
        .unwrap();
    let hidden_lib_id: domain::LibraryId = hidden_lib.id.parse().unwrap();

    publish_s2e3(&store, media.id, &hidden_root);

    insert_s2_subscribe(&store, admin_id, media.id, filter_id, Some(hidden_lib_id));
    insert_s2_subscribe(&store, member_id, media.id, filter_id, Some(hidden_lib_id));

    let admin_snaps = load_arrivals(&store, admin_id).unwrap();
    assert_eq!(admin_snaps[0].rows.len(), 1);
    let now = crate::job_loop::unix_now();
    let items = build_arrivals(admin_snaps, &StubSeason2Catalog, now);
    assert!(
        items
            .iter()
            .any(|i| i["status"] == "imported" && i["episode"] == 3)
    );
    assert!(load_arrivals(&store, member_id).unwrap()[0].rows.is_empty());
    store.begin_library_removal(&hidden_lib.id).unwrap();
    assert!(load_arrivals(&store, admin_id).unwrap()[0].rows.is_empty());
}

#[test]
fn untargeted_subscription_checks_owner_auto_route_and_viewer_access() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, admin_id, member_id, filter_id, media) = seed_arrivals_store(&tmp);
    let default_root = tmp.path().join("tv-default-owner");
    std::fs::create_dir_all(&default_root).unwrap();

    let default_lib = store
        .create_library(
            MediaKind::Tv,
            "成员可见但管理员隐藏默认库",
            &[default_root.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
    store.set_default_library(&default_lib.id).unwrap();
    publish_s2e3(&store, media.id, &default_root);
    insert_s2_subscribe(&store, member_id, media.id, filter_id, None);

    let member_snaps = load_arrivals(&store, member_id).unwrap();
    assert_eq!(member_snaps[0].rows.len(), 1);

    let admin_snaps = load_arrivals(&store, admin_id).unwrap();
    assert_eq!(
        admin_snaps[0].rows.len(),
        1,
        "admin viewing member's untargeted subscription must see imported coverage even when admin_visible=false"
    );

    store
        .update_library(&default_lib.id, None, None, Some(("selected", true, &[])))
        .unwrap();
    let admin_restricted = load_arrivals(&store, admin_id).unwrap();
    assert!(
        admin_restricted[0].rows.is_empty(),
        "admin must not attribute selected default library coverage to unauthorized member's untargeted subscription"
    );
}
