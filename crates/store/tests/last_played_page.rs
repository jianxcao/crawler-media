//! LastPlayed segment ordering through public playback mutations.

use domain::{LibraryId, Media, MediaId, MediaKind, UserId};
use store::Store;
use store::playback::PlaybackMutation;
use store::test_support::{FixtureFile, publish_fixture_file};

fn library(store: &Store, root: &std::path::Path, name: &str) -> LibraryId {
    std::fs::create_dir_all(root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    lib.id.parse().unwrap()
}

fn show(store: &Store, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year: None,
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

fn episode(store: &Store, media: MediaId, root: &std::path::Path, ep: u32) {
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media,
            path: root
                .join(format!("{media}-e{ep}.mkv"))
                .display()
                .to_string(),
            season: Some(1),
            episode: Some(ep),
            ..Default::default()
        },
    )
    .unwrap();
}

fn favorite(store: &Store, user: UserId, media: MediaId, s: i32, e: i32, at: i64) {
    store
        .mutate_playback(
            media,
            &PlaybackMutation::marks(user, s, e, None, Some(true)),
            at,
        )
        .unwrap();
}

#[test]
fn last_played_pages_cross_from_played_to_unplayed() {
    use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let root = tmp.path().join("a");
    let lib = library(&store, &root, "A");
    let user = UserId::new();
    let mut expected = Vec::new();
    for index in 0..19 {
        let media = show(&store, &format!("Item {index}"));
        episode(&store, media, &root, 1);
        let at = if index < 8 { 100 + (index % 3) } else { 0 };
        if index < 8 {
            activity(&store, user, media, at);
        } else if index < 12 {
            favorite(&store, user, media, 1, 1, 1_000);
        }
        expected.push((at, media));
    }
    expected.sort_by_key(|(time, id)| (*time, id.to_string()));
    for descending in [false, true] {
        let mut want = expected.clone();
        if descending {
            want.reverse();
        }
        let mut after = None;
        let mut got = Vec::new();
        loop {
            let query = LibraryQuery {
                library_id: lib,
                user_id: Some(user),
                filter: BrowseFilter::default(),
                order: BrowseOrder::LastPlayed,
                descending,
                page: BrowsePage::Cursor {
                    after: after.clone(),
                    limit: 5,
                },
                include_total: after.is_none(),
            };
            let page = store.library_item_page(&query).unwrap();
            if after.is_none() {
                assert_eq!(page.total, Some(19));
            }
            got.extend(page.items.into_iter().map(|row| row.media_id));
            let Some(next) = page.next_cursor else { break };
            assert_ne!(after.as_deref(), Some(next.as_str()));
            after = Some(next);
        }
        assert_eq!(got, want.iter().map(|(_, id)| *id).collect::<Vec<_>>());
    }
}

#[test]
fn last_played_sort_has_no_temp_btree() {
    use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let root = tmp.path().join("a");
    let lib = library(&store, &root, "A");
    let user = UserId::new();
    for index in 0..80 {
        let media = show(&store, &format!("Item {index}"));
        episode(&store, media, &root, 1);
        if index < 20 {
            activity(&store, user, media, index + 1);
        }
    }
    for descending in [false, true] {
        let query = LibraryQuery {
            library_id: lib,
            user_id: Some(user),
            filter: BrowseFilter::default(),
            order: BrowseOrder::LastPlayed,
            descending,
            page: BrowsePage::Cursor {
                after: None,
                limit: 10,
            },
            include_total: false,
        };
        let (_, stats) = store.library_item_page_with_stats(&query).unwrap();
        assert_eq!(stats.sort_operations, 0, "{stats:?}");
        assert!(stats.fullscan_steps <= 10, "{stats:?}");
    }
}

fn activity(store: &Store, user: UserId, media: MediaId, at: i64) {
    store
        .upsert_unit(
            user,
            media,
            1,
            1,
            30_000,
            Some(false),
            None,
            Some(120_000),
            None,
            None,
            true,
            at,
        )
        .unwrap();
}

fn mark_show(store: &Store, user: UserId, media: MediaId, at: i64) {
    store
        .mutate_playback(
            media,
            &PlaybackMutation::marks(user, -1, -1, Some(true), None),
            at * 1000,
        )
        .unwrap();
}

#[test]
fn last_played_tiers_keep_segment_tie_directions_and_watch_filters() {
    use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("a");
    let lib = library(&store, &root, "A");
    let user = UserId::new();
    let expected = tier_fixtures(&store, user, &root);
    for descending in [false, true] {
        for unwatched_first in [false, true] {
            for watch in [None, Some("seen"), Some("played"), Some("unwatched")] {
                let mut want: Vec<_> = expected
                    .iter()
                    .filter(|(tier, _, _)| match watch {
                        Some("seen") => *tier > 0,
                        Some("played") => *tier == 2,
                        Some("unwatched") => *tier == 0,
                        _ => true,
                    })
                    .copied()
                    .collect();
                want.sort_by(|a, b| {
                    let tier = if unwatched_first {
                        a.0.cmp(&b.0)
                    } else {
                        std::cmp::Ordering::Equal
                    };
                    let rest = a.1.cmp(&b.1).then(a.2.to_string().cmp(&b.2.to_string()));
                    tier.then(if descending { rest.reverse() } else { rest })
                });
                let base = LibraryQuery {
                    library_id: lib,
                    user_id: Some(user),
                    filter: BrowseFilter {
                        unwatched_first,
                        watch: watch.map(str::to_string),
                        ..Default::default()
                    },
                    order: BrowseOrder::LastPlayed,
                    descending,
                    page: BrowsePage::Cursor {
                        after: None,
                        limit: 3,
                    },
                    include_total: true,
                };
                let mut after = None;
                let mut got = Vec::new();
                loop {
                    let mut q = base.clone();
                    q.page = BrowsePage::Cursor {
                        after: after.clone(),
                        limit: 3,
                    };
                    q.include_total = after.is_none();
                    let page = store.library_item_page(&q).unwrap();
                    if after.is_none() {
                        assert_eq!(page.total, Some(want.len() as u64));
                    } else {
                        assert_eq!(page.total, None);
                    }
                    got.extend(page.items.into_iter().map(|row| row.media_id));
                    let Some(next) = page.next_cursor else { break };
                    assert_ne!(after.as_deref(), Some(next.as_str()));
                    after = Some(next);
                    assert!(
                        got.len() <= expected.len(),
                        "cursor cycle must not repeat rows"
                    );
                }
                let wanted: Vec<_> = want.iter().map(|row| row.2).collect();
                assert_eq!(
                    got, wanted,
                    "desc={descending} tier={unwatched_first} watch={watch:?}"
                );
                let mut offset = base.clone();
                offset.page = BrowsePage::Offset { start: 2, limit: 5 };
                let page = store.library_item_page(&offset).unwrap();
                assert_eq!(
                    page.items
                        .iter()
                        .map(|row| row.media_id)
                        .collect::<Vec<_>>(),
                    wanted.iter().skip(2).take(5).copied().collect::<Vec<_>>()
                );
            }
        }
    }
}

fn tier_fixtures(store: &Store, user: UserId, root: &std::path::Path) -> Vec<(i32, i64, MediaId)> {
    let mut expected = Vec::new();
    for index in 0..18 {
        let media = show(store, &format!("Item {index}"));
        episode(store, media, root, 1);
        let (tier, time) = match index % 6 {
            0 => (0, 0),
            1 => {
                favorite(store, user, media, 1, 1, 1000);
                (0, 0)
            }
            2 => {
                activity(store, user, media, 100);
                (1, 100)
            }
            3 => {
                activity(store, user, media, 200);
                (1, 200)
            }
            4 => {
                activity(store, user, media, 50);
                mark_show(store, user, media, 60);
                (2, 50)
            }
            _ => {
                mark_show(store, user, media, 60);
                (2, 0)
            }
        };
        expected.push((tier, time, media));
    }
    expected
}

#[test]
fn last_played_titles_and_bucket_cursors_match_pages_with_and_without_user() {
    use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("a");
    let lib = library(&store, &root, "A");
    let user = UserId::new();
    let mut expected = Vec::new();
    for n in 0..12 {
        let title = format!("{} item", char::from(b'A' + n));
        let media = show(&store, &title);
        episode(&store, media, &root, 1);
        let time = if n < 6 { 100 } else { 0 };
        if time > 0 {
            activity(&store, user, media, time);
        } else if n % 2 == 0 {
            favorite(&store, user, media, 1, 1, 1000);
        }
        expected.push((i32::from(time > 0), time, media, title));
    }
    for current_user in [None, Some(user)] {
        for descending in [false, true] {
            for unwatched_first in [false, true] {
                let mut want = expected.clone();
                want.sort_by(|a, b| {
                    let tier = if current_user.is_some() && unwatched_first {
                        a.0.cmp(&b.0)
                    } else {
                        std::cmp::Ordering::Equal
                    };
                    let activity = if current_user.is_some() {
                        a.1.cmp(&b.1)
                    } else {
                        std::cmp::Ordering::Equal
                    };
                    let rest = activity.then(a.2.to_string().cmp(&b.2.to_string()));
                    tier.then(if descending { rest.reverse() } else { rest })
                });
                let query = LibraryQuery {
                    library_id: lib,
                    user_id: current_user,
                    filter: BrowseFilter {
                        unwatched_first,
                        ..Default::default()
                    },
                    order: BrowseOrder::LastPlayed,
                    descending,
                    page: BrowsePage::Cursor {
                        after: None,
                        limit: 2,
                    },
                    include_total: false,
                };
                assert_eq!(
                    store.library_item_titles(&query).unwrap(),
                    want.iter().map(|row| row.3.clone()).collect::<Vec<_>>()
                );
                let buckets = store.library_item_index(&query).unwrap();
                assert_eq!(buckets.len(), want.len());
                for (bucket, wanted) in buckets.iter().zip(&want) {
                    assert_eq!(bucket.count, 1);
                    let mut at_bucket = query.clone();
                    at_bucket.page = BrowsePage::Cursor {
                        after: bucket.cursor.clone(),
                        limit: 1,
                    };
                    let (page, stats) = store.library_item_page_with_stats(&at_bucket).unwrap();
                    assert_eq!(page.items[0].media_id, wanted.2);
                    assert_eq!(stats.sort_operations, 0, "{stats:?}");
                }
            }
        }
    }
}
