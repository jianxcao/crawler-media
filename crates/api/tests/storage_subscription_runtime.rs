use domain::{Coverage, FetchMode, FilterId, MediaId, MediaKind, Subscribe, SubscribeId, UserId};
use store::Store;

#[test]
fn subscription_with_units_and_retention_alignment() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = MediaId::new();
    let user_id = UserId::new();
    let filter_id = FilterId::new();

    // 1. Create a library
    let lib = store
        .create_library(
            MediaKind::Tv,
            "Anime TV",
            &["/anime"],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    // 2. Insert a subscribe record tied to this library
    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id,
        media_id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(12),
        },
        fetch_mode: FetchMode::Both,
        filter_id,
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(lib.id.parse().unwrap()),
        tracking_state: "active".into(),
        follow_future: true,
        search_interval_secs: 1800,
    };

    store.insert_subscribe(&sub).unwrap();

    let fetched = store
        .get_subscribe(sub.id)
        .unwrap()
        .expect("subscribe exists");
    assert_eq!(fetched.library_id, Some(lib.id.parse().unwrap()));
    assert_eq!(fetched.tracking_state, "active");
}

#[test]
fn file_without_coverage_does_not_invent_subscribe_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "No coverage".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("movie.mkv");
    std::fs::write(&path, b"video").unwrap();
    let file = store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: library.id.parse().unwrap(),
                config_revision: 0,
            },
            &[store::library_write::FilePublication {
                file: &file,
                units: &[],
                expected_version: Some(&file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();
    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(library.id.parse().unwrap()),
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let snapshot = store
        .subscribe_library_snapshot(&sub, MediaKind::Movie)
        .unwrap();
    let facts =
        api::subscribe_library_read::evaluate_subscribe_facts_out_of_lock(&snapshot).unwrap();
    assert!(
        facts.movie().is_none(),
        "a physical file does not imply whole-work coverage"
    );
}
