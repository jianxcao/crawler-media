//! Scoped TV episode panorama regression tests: empty library guard,
//! `library_removals` hiding, default TV `NULL` library binding, and `subscriber_filter`.

use domain::{
    Confidence, Coverage, FetchMode, LibraryId, Media, MediaId, MediaKind, Release, Subscribe,
    SubscribeId, UserId,
};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{EpisodeAvailabilityStatus, Store};

fn noop_parse(title: &str) -> Release {
    Release {
        title: title.to_string(),
        year: None,
        season: None,
        episode: None,
        episode_to: None,
        resolution: None,
        source: None,
        codec: None,
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: Confidence::High,
        season_inferred: false,
    }
}

fn make_subscribe(
    store: &Store,
    user_id: UserId,
    media_id: MediaId,
    library_id: Option<LibraryId>,
) -> SubscribeId {
    let id = SubscribeId::new();
    let filter_id = store.list_filters().unwrap()[0].id;
    store
        .insert_subscribe(&Subscribe {
            id,
            user_id,
            media_id,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: None,
            },
            fetch_mode: FetchMode::Search,
            filter_id,
            wash_cut: false,
            wash_cut_filter_id: None,
            full_season_pack: false,
            downloader_id: None,
            library_id,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
            keep_old_versions: false,
        })
        .unwrap();
    id
}

struct PanoramaFixture {
    store: Store,
    default_id: LibraryId,
    default_raw: String,
    alt_id: LibraryId,
    media_id: MediaId,
    user_a: UserId,
}

fn setup_panorama_fixture(root: &std::path::Path) -> PanoramaFixture {
    let store = Store::open(root.join("data")).unwrap();
    let default_tv = store.default_library(MediaKind::Tv).unwrap().unwrap();
    let default_id: LibraryId = default_tv.id.parse().unwrap();

    let alt_root = root.join("alt-tv");
    std::fs::create_dir_all(&alt_root).unwrap();
    let alt_tv = store
        .create_library(
            MediaKind::Tv,
            "Alt TV",
            &[alt_root.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
    let alt_id: LibraryId = alt_tv.id.parse().unwrap();

    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Scoped Show".into(),
            year: Some(2025),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let ep1_path = default_tv.root_paths[0].join("Scoped.Show.S01E01.mkv");
    std::fs::create_dir_all(ep1_path.parent().unwrap()).unwrap();
    std::fs::write(&ep1_path, b"ep1").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: ep1_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();

    let user_a = UserId::new();
    let user_b = UserId::new();
    let sub_null = make_subscribe(&store, user_a, media_id, None);
    store
        .record_wanted_grab(sub_null, Some(1), Some(2), "S01E02.mkv", 1_000)
        .unwrap();
    let sub_alt = make_subscribe(&store, user_b, media_id, Some(alt_id));
    store
        .record_wanted_grab(sub_alt, Some(1), Some(3), "S01E03.mkv", 1_000)
        .unwrap();

    PanoramaFixture {
        store,
        default_id,
        default_raw: default_tv.id,
        alt_id,
        media_id,
        user_a,
    }
}

#[test]
fn episode_panorama_scoped_enforces_library_user_and_removal_boundaries() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let f = setup_panorama_fixture(dir.path());

    let empty = f
        .store
        .episode_panorama_scoped(&[f.media_id], &[], None, "", None, noop_parse)
        .unwrap();
    assert_eq!(
        empty.episode_availability(1, 1, None).status,
        EpisodeAvailabilityStatus::Missing
    );
    assert_eq!(
        empty.episode_availability(1, 2, None).status,
        EpisodeAvailabilityStatus::Missing
    );

    let alt_admin = f
        .store
        .episode_panorama_scoped(
            &[f.media_id],
            &[f.alt_id],
            Some(f.alt_id),
            "",
            None,
            noop_parse,
        )
        .unwrap();
    assert_eq!(
        alt_admin.episode_availability(1, 2, None).status,
        EpisodeAvailabilityStatus::Missing
    );
    assert_eq!(
        alt_admin.episode_availability(1, 3, None).status,
        EpisodeAvailabilityStatus::Downloading
    );

    let alt_user_a = f
        .store
        .episode_panorama_scoped(
            &[f.media_id],
            &[f.alt_id],
            Some(f.alt_id),
            "",
            Some(Some(f.user_a)),
            noop_parse,
        )
        .unwrap();
    assert_eq!(
        alt_user_a.episode_availability(1, 3, None).status,
        EpisodeAvailabilityStatus::Missing
    );

    let alt_anon = f
        .store
        .episode_panorama_scoped(
            &[f.media_id],
            &[f.alt_id],
            Some(f.alt_id),
            "",
            Some(None),
            noop_parse,
        )
        .unwrap();
    assert_eq!(
        alt_anon.episode_availability(1, 3, None).status,
        EpisodeAvailabilityStatus::Missing
    );

    let def_user_a = f
        .store
        .episode_panorama_scoped(
            &[f.media_id],
            &[f.default_id],
            Some(f.default_id),
            "",
            Some(Some(f.user_a)),
            noop_parse,
        )
        .unwrap();
    assert_eq!(
        def_user_a.episode_availability(1, 1, None).status,
        EpisodeAvailabilityStatus::InLibrary
    );
    assert_eq!(
        def_user_a.episode_availability(1, 2, None).status,
        EpisodeAvailabilityStatus::Downloading
    );
    assert_eq!(
        def_user_a.episode_availability(1, 3, None).status,
        EpisodeAvailabilityStatus::Missing
    );

    f.store.begin_library_removal(&f.default_raw).unwrap();
    let after_removal = f
        .store
        .episode_panorama_scoped(
            &[f.media_id],
            &[f.default_id],
            Some(f.default_id),
            "",
            None,
            noop_parse,
        )
        .unwrap();
    assert_eq!(
        after_removal.episode_availability(1, 1, None).status,
        EpisodeAvailabilityStatus::Missing
    );
    assert_eq!(
        after_removal.episode_availability(1, 2, None).status,
        EpisodeAvailabilityStatus::Missing
    );
}
