mod query_model_support;

use domain::UserId;
use query_model_support::{run, setup};
use std::sync::Barrier;

#[test]
fn concurrent_integrity_advances_do_not_duplicate_reports_or_staged_counts() {
    let (tmp, store, library, media) = setup();
    let users = [UserId::new(), UserId::new(), UserId::new()];
    for user in users {
        for id in &media {
            store
                .mutate_playback(
                    id.parse().unwrap(),
                    &store::playback::PlaybackMutation::marks(user, -1, -1, None, Some(true)),
                    1,
                )
                .unwrap();
        }
    }
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db.execute("DELETE FROM user_library_items", []).unwrap();
    db.execute("DELETE FROM library_items", []).unwrap();
    store
        .begin_library_integrity(library, true, "concurrent", 0)
        .unwrap();
    let start = Barrier::new(2);
    std::thread::scope(|scope| {
        let worker = || {
            start.wait();
            for step in 1..10_000 {
                let status = store
                    .advance_library_operation("concurrent", step, 1)
                    .unwrap();
                if status.state == "succeeded" {
                    return;
                }
                assert_eq!(status.state, "applying");
            }
            panic!("concurrent Integrity did not finish");
        };
        let first = scope.spawn(worker);
        let second = scope.spawn(worker);
        first.join().unwrap();
        second.join().unwrap();
    });
    let report = store.library_integrity_report("concurrent").unwrap();
    assert_eq!(report.totals.missing_items, 4);
    assert_eq!(report.totals.missing_user_items, 12);
    assert_eq!(report.totals.mismatched_media, 4);
    assert_eq!(report.missing_user_items.len(), 12);
    for user in users {
        for id in &media {
            assert!(
                store
                    .get_user_library_summary(user, library, id.parse().unwrap())
                    .unwrap()
                    .unwrap()
                    .favorite
            );
        }
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM library_integrity_rows", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    run(&store, library, false, "concurrent-verify");
    assert!(
        !store
            .library_integrity_report("concurrent-verify")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn movie_coverage_keeps_the_existing_continuation_projection() {
    let (_tmp, store, library, media) = setup();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media[0].parse().unwrap(),
            path: "/movies/extra-coordinate.mkv".into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    run(&store, library, false, "movie-coordinate");
    assert!(
        !store
            .library_integrity_report("movie-coordinate")
            .unwrap()
            .has_issues()
    );
}
