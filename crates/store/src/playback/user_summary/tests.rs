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
    store
        .with_read(crate::Db::Library, |conn| {
            Store::existing_summary(conn, user, library_id, media)
        })
        .unwrap()
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
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(user, 1, 1, None, Some(true)),
            1_000_000,
        )
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
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(user, 1, UNIT_WHOLE, Some(true), None),
            2_000,
        )
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
    assert_eq!(row.last_played_at, 0, "a mark is not viewing activity");
}

#[test]
fn season_mark_preserves_other_season_display_while_advancing_global_continuation() {
    let (temp, store) = open();
    let (season_one, season_one_root) = tv_library(&store, &temp, "s1");
    let (season_two, season_two_root) = tv_library(&store, &temp, "s2");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &season_one_root, media, 1, 1);
    seed_episode(&store, &season_two_root, media, 2, 1);
    let user = UserId::new();
    let one_id = season_one.parse().unwrap();
    let two_id = season_two.parse().unwrap();
    store
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(user, 2, 1, None, Some(true)),
            1_000,
        )
        .unwrap();
    let before = store
        .get_user_library_summary(user, two_id, media)
        .unwrap()
        .unwrap();
    let local_display = |row: &UserLibrarySummary| {
        (
            row.seen,
            row.played,
            row.favorite,
            row.last_played_at,
            row.favorite_at,
            row.owned_episode_count,
            row.completed_episode_count,
        )
    };
    assert_eq!(favorite_page(&store, &season_two, user), 1);
    store
        .upsert_unit(
            user,
            media,
            1,
            1,
            30_000,
            None,
            None,
            Some(600_000),
            None,
            None,
            false,
            10,
        )
        .unwrap();
    store
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(user, 1, UNIT_WHOLE, Some(true), None),
            20_000,
        )
        .unwrap();
    let one = store
        .get_user_library_summary(user, one_id, media)
        .unwrap()
        .unwrap();
    assert_eq!(one.completed_episode_count, 1);
    assert!(one.played);
    let after = store
        .get_user_library_summary(user, two_id, media)
        .unwrap()
        .unwrap();
    assert_eq!(local_display(&after), local_display(&before));
    assert_eq!(favorite_page(&store, &season_two, user), 1);
    let next = store
        .next_up_page(&crate::ContinueQuery {
            user_id: user,
            libraries: vec![one_id, two_id],
            parent: domain::ProtocolParent::Root,
            media_id: None,
            season: None,
            limit: 20,
            offset: 0,
        })
        .unwrap();
    assert_eq!(next.total, 1);
    assert_eq!(next.items[0].reference.library_id, two_id);
    assert_eq!(
        next.items[0].reference.item,
        domain::TypedItemId::Unit {
            media,
            key: domain::UnitKey::new(2, 1),
        }
    );
    assert_eq!(next.items[0].last_played_at, 10);
    let home = store
        .home_activity_page(&crate::HomeQuery {
            user_id: user,
            libraries: vec![one_id, two_id],
            limit: 20,
        })
        .unwrap();
    assert_eq!(home.len(), 1);
    assert_eq!(home[0].reference, next.items[0].reference);
    assert_eq!(home[0].last_activity, 10);
    assert!(home[0].advanced);
}

#[test]
fn replaying_the_same_write_is_a_no_op() {
    let (temp, store) = open();
    let (library_id, root) = tv_library(&store, &temp, "tv");
    let media = seed_tv(&store, "Show");
    seed_episode(&store, &root, media, 1, 1);
    let user = UserId::new();

    store
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(user, 1, 1, Some(true), Some(true)),
            3_000,
        )
        .unwrap();
    let first = summary_row(&store, user, &library_id, media).unwrap();
    store
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(user, 1, 1, Some(true), Some(true)),
            3_000,
        )
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
        .mutate_playback(
            media,
            &crate::playback::PlaybackMutation::marks(
                user,
                UNIT_WHOLE,
                UNIT_WHOLE,
                Some(true),
                Some(true),
            ),
            5_000,
        )
        .unwrap();

    assert!(summary_row(&store, user, &owning, media).is_some());
    assert!(summary_row(&store, user, &other, media).is_none());
    let count: i64 = store
        .with_read(crate::Db::Library, |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM user_library_items WHERE library_id = ?1",
                params![&other],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(count, 0);
}
