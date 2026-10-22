use domain::{
    Coverage, FetchMode, FilterId, LibraryId, Media, MediaId, MediaKind, QualitySource, Subscribe,
    SubscribeId, UserId,
};
use store::Store;

#[test]
fn upsert_unit_and_set_unit_marks_satisfies_schema_check_constraints() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let user_id = UserId::new();
    let media_id = MediaId::new();

    let m = Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&m).unwrap();

    // 1. Favorite only episode (no played assertion, position 0) -> R01-A trigger
    store
        .set_unit_favorite(user_id, media_id, 1, 1, true, 100)
        .unwrap();

    // 2. Mark unit played -> Should update has_played_assertion=1, played_changed_at_ms=101
    store
        .set_unit_marks(user_id, media_id, 1, 1, Some(true), None, 101)
        .unwrap();

    let state = store
        .unit_state(user_id, media_id, 1, 1)
        .unwrap()
        .expect("unit state exists");
    assert!(state.played);
    assert!(state.favorite);

    // 3. Upsert unit with position_ms > 0 when no file exists yet -> Must clamp to 0/NULL to satisfy R01-B
    let upserted = store
        .upsert_unit(
            user_id,
            media_id,
            1,
            2,
            1000,
            Some(false),
            None,
            None,
            None,
            None,
            false,
            200,
        )
        .unwrap();

    assert_eq!(upserted.position_ms, 0);
}
