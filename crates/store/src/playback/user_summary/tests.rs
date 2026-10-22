use crate::test_support::FixtureFile;
use std::path::Path;

use super::*;
use crate::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use domain::{Confidence, LedgerId, Media, MediaKind, QualitySource};

fn open() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    (temp, store)
}

fn tv_library(store: &Store, temp: &tempfile::TempDir, name: &str) -> (String, std::path::PathBuf) {
    let root = temp.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    (library.id, root)
}

fn seed_tv(store: &Store, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.to_string(),
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
    media.id
}

fn seed_episode(store: &Store, root: &Path, media: MediaId, season: u32, episode: u32) {
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media,
        path: root
            .join(format!("S{season:02}E{episode:02}.mkv"))
            .display()
            .to_string(),
        season: Some(season),
        episode: Some(episode),
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    crate::test_support::publish_fixture_file(store, &row).unwrap();
}

fn summary_row(
    store: &Store,
    user: UserId,
    library_id: &str,
    media: MediaId,
) -> Option<(i64, Summary)> {
    Store::existing_summary(&store.library, user, library_id, media).unwrap()
}

fn favorite_page(store: &Store, library_id: &str, user: UserId) -> usize {
    let query = LibraryQuery {
        library_id: library_id.parse().unwrap(),
        user_id: Some(user),
        filter: BrowseFilter {
            watch: Some("favorite".into()),
            ..BrowseFilter::default()
        },
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 50,
        },
        include_total: true,
    };
    store.library_item_page(&query).unwrap().items.len()
}

#[test]
fn favouriting_one_episode_marks_the_media_favourite_in_its_library() {
    let (temp, store) = open();
    let (library_id, root) = tv_library(&store, &temp, "tv");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &root, media, 1, 1);
    seed_episode(&store, &root, media, 1, 2);
    let user = UserId::new();

    store
        .set_unit_favorite(user, media, 1, 1, true, 1_000)
        .unwrap();

    let (_, row) =
        summary_row(&store, user, &library_id, media).expect("a favourite creates a summary row");
    assert!(row.favorite);
    assert_eq!(row.favorite_at, 1_000);
    assert!(!row.seen, "a favourite alone is not a viewing fact");
    assert_eq!(favorite_page(&store, &library_id, user), 1);
}

#[test]
fn season_mark_completes_the_owned_season_and_advances_next_up() {
    let (temp, store) = open();
    let (library_id, root) = tv_library(&store, &temp, "tv");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &root, media, 1, 1);
    seed_episode(&store, &root, media, 1, 2);
    seed_episode(&store, &root, media, 2, 1);
    let user = UserId::new();

    store
        .set_unit_marks(user, media, 1, UNIT_WHOLE, Some(true), None, 2_000)
        .unwrap();

    let (_, row) =
        summary_row(&store, user, &library_id, media).expect("a mark creates a summary row");
    assert_eq!(row.owned_episode_count, 3);
    assert_eq!(
        row.completed_episode_count, 2,
        "both owned S01 episodes are complete"
    );
    assert!(row.seen);
    assert!(!row.played, "S02 is still unwatched");
    assert_eq!((row.next_season, row.next_episode), (Some(2), Some(1)));
    assert_eq!(row.last_played_at, 2_000);
}

#[test]
fn season_mark_leaves_libraries_owning_other_seasons_untouched() {
    let (temp, store) = open();
    let (season_one, season_one_root) = tv_library(&store, &temp, "s1");
    let (season_two, season_two_root) = tv_library(&store, &temp, "s2");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &season_one_root, media, 1, 1);
    seed_episode(&store, &season_two_root, media, 2, 1);
    let user = UserId::new();

    // Give the S02 Library a row of its own, then plant a sentinel: a
    // season-1 write must not touch it at all.
    store
        .set_unit_favorite(user, media, 2, 1, true, 1_000)
        .unwrap();
    store
        .library
        .execute(
            "UPDATE user_library_items SET last_played_at = 7_777, summary_revision = 9
                 WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3",
            params![user.to_string(), &season_two, media.to_string()],
        )
        .unwrap();

    store
        .set_unit_marks(user, media, 1, UNIT_WHOLE, Some(true), None, 2_000)
        .unwrap();

    let (_, one) = summary_row(&store, user, &season_one, media).expect("S01 library updated");
    assert_eq!(one.completed_episode_count, 1);
    assert!(one.played);

    let (revision, two) = summary_row(&store, user, &season_two, media).expect("S02 row survives");
    assert_eq!(revision, 9);
    assert_eq!(two.last_played_at, 7_777);
}

#[test]
fn replaying_the_same_write_is_a_no_op() {
    let (temp, store) = open();
    let (library_id, root) = tv_library(&store, &temp, "tv");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &root, media, 1, 1);
    let user = UserId::new();

    store
        .set_unit_marks(user, media, 1, 1, Some(true), Some(true), 3_000)
        .unwrap();
    let first = summary_row(&store, user, &library_id, media).unwrap();
    store
        .set_unit_marks(user, media, 1, 1, Some(true), Some(true), 3_000)
        .unwrap();
    assert_eq!(
        summary_row(&store, user, &library_id, media).unwrap(),
        first,
        "repeating a mark must not bump the revision"
    );

    store
        .upsert_unit(
            user,
            media,
            1,
            1,
            90_000,
            None,
            None,
            Some(600_000),
            None,
            None,
            false,
            4_000,
        )
        .unwrap();
    let with_progress = summary_row(&store, user, &library_id, media).unwrap();
    store
        .upsert_unit(
            user,
            media,
            1,
            1,
            90_000,
            None,
            None,
            Some(600_000),
            None,
            None,
            false,
            4_000,
        )
        .unwrap();
    assert_eq!(
        summary_row(&store, user, &library_id, media).unwrap(),
        with_progress,
        "repeating the same progress must be a no-op"
    );
    assert!(with_progress.1.seen);
}

#[test]
fn a_library_that_does_not_own_the_media_gets_no_summary() {
    let (temp, store) = open();
    let (owning, root) = tv_library(&store, &temp, "owning");
    let (other, _) = tv_library(&store, &temp, "other");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &root, media, 1, 1);
    let user = UserId::new();

    store
        .set_unit_marks(
            user,
            media,
            UNIT_WHOLE,
            UNIT_WHOLE,
            Some(true),
            Some(true),
            5_000,
        )
        .unwrap();

    assert!(summary_row(&store, user, &owning, media).is_some());
    assert!(summary_row(&store, user, &other, media).is_none());
    let count: i64 = store
        .library
        .query_row(
            "SELECT COUNT(*) FROM user_library_items WHERE library_id = ?1",
            params![&other],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
