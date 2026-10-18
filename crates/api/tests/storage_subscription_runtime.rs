use domain::{Coverage, FetchMode, FilterId, LibraryId, MediaId, MediaKind, Subscribe, SubscribeId, UserId};
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
        .create_library(MediaKind::Tv, "Anime TV", &["/anime"], "everyone", true, &[])
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

    let fetched = store.get_subscribe(sub.id).unwrap().expect("subscribe exists");
    assert_eq!(fetched.library_id, Some(lib.id.parse().unwrap()));
    assert_eq!(fetched.tracking_state, "active");
}
