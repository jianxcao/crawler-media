mod query_model_support;

use domain::UserId;
use query_model_support::setup;
use store::{FavoriteOrder, PlaybackViewQuery, StoreError};

fn query(user: UserId, library: domain::LibraryId) -> PlaybackViewQuery {
    PlaybackViewQuery {
        user,
        libraries: vec![library],
        limit: 2,
        after: None,
        order: FavoriteOrder::Title,
        descending: false,
        unwatched_first: true,
    }
}

#[test]
fn favorite_pages_and_positions_reject_frozen_projection_scope() {
    let (_tmp, store, library, ids) = setup();
    let user = UserId::new();
    store
        .mutate_playback(
            ids[0].parse().unwrap(),
            &store::playback::PlaybackMutation::marks(user, -1, -1, None, Some(true)),
            1,
        )
        .unwrap();
    let query = query(user, library);
    assert_eq!(store.playback_favorite_page(&query).unwrap().total, Some(1));
    store
        .begin_library_integrity(library, true, "favorite-owner", 2)
        .unwrap();
    store
        .advance_library_operation("favorite-owner", 3, 1)
        .unwrap();

    assert!(matches!(
        store.playback_favorite_page(&query),
        Err(StoreError::QueryUnavailable { library_id }) if library_id == library
    ));
    for position in [0, 1] {
        assert!(matches!(
            store.playback_favorite_position(&query, position),
            Err(StoreError::QueryUnavailable { library_id }) if library_id == library
        ));
    }
}

#[test]
fn roster_statistics_reject_partial_integrity_repairs() {
    let (_tmp, store, library, _) = setup();
    assert_eq!(
        store
            .library_roster_stats(&library.to_string())
            .unwrap()
            .item_count,
        4
    );
    store
        .begin_library_integrity(library, true, "roster-owner", 0)
        .unwrap();
    store
        .advance_library_operation("roster-owner", 1, 1)
        .unwrap();
    assert!(matches!(
        store.library_roster_stats(&library.to_string()),
        Err(StoreError::QueryUnavailable { library_id }) if library_id == library
    ));
}

#[test]
fn favorite_and_roster_reads_reject_a_completed_failed_integrity_check() {
    let (tmp, store, library, ids) = setup();
    let user = UserId::new();
    store
        .mutate_playback(
            ids[0].parse().unwrap(),
            &store::playback::PlaybackMutation::marks(user, -1, -1, None, Some(true)),
            1,
        )
        .unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE library_items SET title='Corrupt' WHERE library_id=?1",
        [library.to_string()],
    )
    .unwrap();
    store
        .begin_library_integrity(library, false, "failed-check", 0)
        .unwrap();
    for step in 1..1_000 {
        if store
            .advance_library_operation("failed-check", step, 1)
            .unwrap()
            .state
            == "succeeded"
        {
            break;
        }
    }
    assert!(
        store
            .library_integrity_report("failed-check")
            .unwrap()
            .has_issues()
    );
    for result in [
        store
            .playback_favorite_page(&query(user, library))
            .map(|_| ()),
        store
            .playback_favorite_position(&query(user, library), 0)
            .map(|_| ()),
        store.library_roster_stats(&library.to_string()).map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(StoreError::Protected(message)) if message.contains("library.integrity"))
        );
    }
}
