//! Public logical-favorite semantics and bounded candidate work.

use domain::{LibraryId, Media, MediaId, MediaKind, UserId};
use store::playback::PlaybackMutation;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{FavoriteOrder, PlaybackViewQuery, Store};

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

fn query(
    user: UserId,
    libraries: Vec<LibraryId>,
    limit: usize,
    after: Option<String>,
) -> PlaybackViewQuery {
    PlaybackViewQuery {
        user,
        libraries,
        limit,
        after,
        order: FavoriteOrder::FavoritedAt,
        descending: true,
        unwatched_first: false,
    }
}

#[test]
fn favorites_keep_global_unit_semantics_when_only_other_episode_is_visible() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let ra = tmp.path().join("a");
    let rb = tmp.path().join("b");
    let a = library(&store, &ra, "visible");
    let _b = library(&store, &rb, "not-in-authorized-scope");
    let media = show(&store, "Shared");
    episode(&store, media, &ra, 1);
    episode(&store, media, &rb, 2);
    let user = UserId::new();
    favorite(&store, user, media, 1, 2, 10_000);
    let page = store
        .playback_favorite_page(&query(user, vec![a], 10, None))
        .unwrap();
    assert_eq!(page.total, Some(1));
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].library_id, a);
    assert_eq!(
        (page.items[0].unit.season, page.items[0].unit.episode),
        (1, 2)
    );
    assert_eq!(page.items[0].favorited_at, 10);
    // A newer favorite coordinate that no currently published file owns also
    // remains the logical Media favorite, as the existing API does.
    favorite(&store, user, media, 3, 4, 11_000);
    let page = store
        .playback_favorite_page(&query(user, vec![a], 10, None))
        .unwrap();
    assert_eq!(
        (page.items[0].unit.season, page.items[0].unit.episode),
        (3, 4)
    );
}

#[test]
fn favorites_keyset_pages_are_disjoint_and_complete() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("a");
    let a = library(&store, &root, "A");
    let user = UserId::new();
    let mut ids = Vec::new();
    for index in 0..25 {
        let media = show(&store, &format!("Favorite {index}"));
        episode(&store, media, &root, 1);
        episode(&store, media, &root, 2);
        favorite(&store, user, media, 1, 1, 10_000);
        favorite(&store, user, media, 1, 2, 10_000);
        ids.push(media);
    }
    ids.sort_by_key(ToString::to_string);
    let mut found = Vec::new();
    let mut after = None;
    for expected_size in [10, 10, 5] {
        let page = store
            .playback_favorite_page(&query(user, vec![a], 10, after.clone()))
            .unwrap();
        assert_eq!(page.total, after.is_none().then_some(25));
        assert_eq!(page.items.len(), expected_size);
        after = page.next_cursor;
        assert!(page.items.iter().all(|row| row.unit.episode == 2));
        found.extend(page.items.into_iter().map(|row| row.media_id));
    }
    assert!(after.is_none());
    assert_eq!(found, ids, "favorite DESC ties still use Media ASC");
}

#[test]
fn favorites_page_plan_has_no_scan() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("a");
    let a = library(&store, &root, "A");
    let user = UserId::new();
    let target = show(&store, "Target");
    episode(&store, target, &root, 1);
    favorite(&store, user, target, 1, 1, 10_000);
    let measure = || {
        store.with_stats(store::stats::StatsScope::Library, || {
            let page = store
                .playback_favorite_page(&query(user, vec![a], 10, None))
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].media_id, target);
        })
    };
    let (_, small) = measure();
    for index in 0..160 {
        let media = show(&store, &format!("Unrelated {index}"));
        episode(&store, media, &root, 1);
    }
    let (_, large) = measure();
    assert_eq!(small.sort_operations, 0, "{small:?}");
    assert_eq!(large.sort_operations, 0, "{large:?}");
    assert!(
        large.fullscan_steps <= small.fullscan_steps + 8,
        "{small:?} -> {large:?}"
    );
    assert!(
        large.vm_steps <= small.vm_steps + 800,
        "{small:?} -> {large:?}"
    );
}

fn set_favorite(store: &Store, user: UserId, media: MediaId, s: i32, e: i32, on: bool, at: i64) {
    store
        .mutate_playback(
            media,
            &PlaybackMutation::marks(user, s, e, None, Some(on)),
            at,
        )
        .unwrap();
}

#[test]
fn favorite_global_unit_winner_lifecycle_and_unknown_timestamp() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("visible");
    let a = library(&store, &root, "Visible");
    let user = UserId::new();
    let target = show(&store, "Logical global favorite");
    episode(&store, target, &root, 1);
    favorite(&store, user, target, 1, 1, 10_000);
    favorite(&store, user, target, 1, 1, 20_000);
    let read = || {
        store
            .playback_favorite_page(&query(user, vec![a], 10, None))
            .unwrap()
    };
    assert_eq!(
        read().items[0].favorited_at,
        10,
        "repeat favorite preserves original time"
    );
    favorite(&store, user, target, 5, 7, 30_000);
    favorite(&store, user, target, 4, 99, 30_000);
    assert_eq!(
        (read().items[0].unit.season, read().items[0].unit.episode),
        (5, 7)
    );
    set_favorite(&store, user, target, 5, 7, false, 40_000);
    assert_eq!(
        (read().items[0].unit.season, read().items[0].unit.episode),
        (4, 99)
    );
    set_favorite(&store, user, target, 4, 99, false, 40_000);
    assert_eq!(
        read().items[0].favorited_at,
        10,
        "other unit's logical favorite remains"
    );
    set_favorite(&store, user, target, 1, 1, false, 40_000);
    assert_eq!(read().total, Some(0));
    favorite(&store, user, target, 1, 1, 50_000);
    assert_eq!(
        read().items[0].favorited_at,
        50,
        "refavorite records a new time"
    );
    let unknown = show(&store, "Timestamp unknown");
    episode(&store, unknown, &root, 1);
    favorite(&store, user, unknown, -1, -1, 0);
    let page = read();
    assert_eq!(page.total, Some(2));
    assert_eq!(page.items[1].media_id, unknown);
    assert_eq!(page.items[1].favorited_at, 0);
    assert!(
        store
            .playback_favorite_page(&query(UserId::new(), vec![a], 10, None))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .playback_favorite_page(&query(user, vec![], 10, None))
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn favorite_unwatched_tiers_aggregate_visible_libraries_before_paging() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root_a = tmp.path().join("a");
    let root_b = tmp.path().join("b");
    let a = library(&store, &root_a, "A");
    let b = library(&store, &root_b, "B");
    let user = UserId::new();
    let unseen = show(&store, "Unseen oldest favorite");
    let partial = show(&store, "Watching middle favorite");
    let played = show(&store, "Played newest favorite");
    for id in [unseen, partial, played] {
        episode(&store, id, &root_a, 1);
        episode(&store, id, &root_b, 2);
    }
    favorite(&store, user, unseen, 8, 9, 100_000);
    favorite(&store, user, partial, 8, 9, 200_000);
    favorite(&store, user, played, 8, 9, 300_000);
    store
        .upsert_unit(
            user,
            partial,
            1,
            1,
            30_000,
            Some(false),
            None,
            Some(120_000),
            None,
            None,
            false,
            10,
        )
        .unwrap();
    store
        .mutate_playback(
            played,
            &PlaybackMutation::marks(user, 1, 1, Some(true), None),
            10_000,
        )
        .unwrap();
    let mut q = query(user, vec![a, b], 1, None);
    q.unwatched_first = true;
    let mut got = Vec::new();
    let source = if a.to_string() < b.to_string() { a } else { b };
    for _ in 0..3 {
        let page = store.playback_favorite_page(&q).unwrap();
        assert_eq!(page.total, q.after.is_none().then_some(3));
        q.after = page.next_cursor;
        assert_eq!(page.items[0].library_id, source);
        assert_eq!(
            (page.items[0].unit.season, page.items[0].unit.episode),
            (8, 9)
        );
        got.push(page.items[0].media_id);
    }
    assert_eq!(got, [unseen, partial, played]);
    q.unwatched_first = false;
    q.after = None;
    q.limit = 10;
    assert_eq!(
        store
            .playback_favorite_page(&q)
            .unwrap()
            .items
            .iter()
            .map(|row| row.media_id)
            .collect::<Vec<_>>(),
        [played, partial, unseen]
    );
}

#[test]
fn favorite_position_seek_and_cursor_scope_preserve_real_window_navigation() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("visible");
    let a = library(&store, &root, "Visible");
    let user = UserId::new();
    let mut ids = Vec::new();
    for index in 0..25 {
        let id = show(&store, &format!("Item {index}"));
        episode(&store, id, &root, 1);
        favorite(&store, user, id, 1, 1, 10_000);
        ids.push(id);
    }
    ids.sort_by_key(ToString::to_string);
    let mut q = query(user, vec![a], 5, None);
    assert!(store.playback_favorite_position(&q, 0).unwrap().is_none());
    let after = store.playback_favorite_position(&q, 13).unwrap().unwrap();
    q.after = Some(after.clone());
    let page = store.playback_favorite_page(&q).unwrap();
    assert_eq!(page.total, None);
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.media_id)
            .collect::<Vec<_>>(),
        ids[13..18]
    );
    assert!(page.next_cursor.is_some());
    for changed in 0..5 {
        let mut wrong = q.clone();
        match changed {
            0 => wrong.user = UserId::new(),
            1 => wrong.libraries = vec![],
            2 => wrong.order = FavoriteOrder::Title,
            3 => wrong.descending = false,
            _ => wrong.unwatched_first = true,
        }
        assert!(matches!(
            store.playback_favorite_page(&wrong),
            Err(store::StoreError::Invalid(_))
        ));
    }
    assert!(matches!(
        store.playback_favorite_position(&q, 25),
        Err(store::StoreError::Invalid(_))
    ));
    q.after = None;
    q.after = store.playback_favorite_position(&q, 25).unwrap();
    let end = store.playback_favorite_page(&q).unwrap();
    assert!(end.items.is_empty());
    assert_eq!(end.total, None);
    assert!(end.next_cursor.is_none());
    q.after = Some("malformed".into());
    assert!(matches!(
        store.playback_favorite_page(&q),
        Err(store::StoreError::Invalid(_))
    ));
}

#[test]
fn favorite_cursor_orders_preserve_ascending_ties_and_null_years() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("visible");
    let library = library(&store, &root, "Visible");
    let user = UserId::new();
    let mut expected = Vec::new();
    for (title, year, at) in [
        ("Bravo", None, 0),
        ("Alpha", Some(2000), 1_000),
        ("Alpha", None, 1_000),
        ("Charlie", Some(2001), 2_000),
        ("Bravo", Some(2000), 0),
        ("Delta", Some(2001), 2_000),
    ] {
        let id = show(&store, title);
        let mut media = store.get_media(id).unwrap().unwrap();
        media.year = year;
        store.update_media(&media).unwrap();
        episode(&store, id, &root, 1);
        favorite(&store, user, id, 1, 1, at);
        expected.push((id, title.to_lowercase(), year, at / 1000));
    }
    for order in [
        FavoriteOrder::FavoritedAt,
        FavoriteOrder::Title,
        FavoriteOrder::Year,
    ] {
        for descending in [false, true] {
            let mut want = expected.clone();
            want.sort_by(|a, b| {
                let primary = match order {
                    FavoriteOrder::FavoritedAt => a.3.cmp(&b.3),
                    FavoriteOrder::Title => a.1.cmp(&b.1),
                    FavoriteOrder::Year => a.2.cmp(&b.2),
                };
                let primary = if descending {
                    primary.reverse()
                } else {
                    primary
                };
                primary.then_with(|| a.0.to_string().cmp(&b.0.to_string()))
            });
            let want = want.into_iter().map(|row| row.0).collect::<Vec<_>>();
            let mut q = query(user, vec![library], 2, None);
            q.order = order;
            q.descending = descending;
            let mut actual = Vec::new();
            for _ in 0..3 {
                let page = store.playback_favorite_page(&q).unwrap();
                assert_eq!(page.total, q.after.is_none().then_some(6));
                actual.extend(page.items.into_iter().map(|row| row.media_id));
                q.after = page.next_cursor;
            }
            assert!(q.after.is_none());
            assert_eq!(actual, want, "{order:?}, descending={descending}");
            q.after = None;
            q.after = store.playback_favorite_position(&q, 3).unwrap();
            let page = store.playback_favorite_page(&q).unwrap();
            assert_eq!(page.total, None);
            assert_eq!(
                page.items
                    .iter()
                    .map(|row| row.media_id)
                    .collect::<Vec<_>>(),
                want[3..5]
            );
        }
    }
}

#[test]
fn favorite_continuation_skips_the_exact_total_statement() {
    let tmp = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("visible");
    let library = library(&store, &root, "Visible");
    let user = UserId::new();
    for index in 0..5 {
        let id = show(&store, &format!("Item {index}"));
        episode(&store, id, &root, 1);
        favorite(&store, user, id, 1, 1, 1_000);
    }
    let mut q = query(user, vec![library], 2, None);
    let (head, head_work) = store.with_stats(store::stats::StatsScope::Library, || {
        store.playback_favorite_page(&q).unwrap()
    });
    assert_eq!(head.total, Some(5));
    q.after = head.next_cursor;
    let (next, next_work) = store.with_stats(store::stats::StatsScope::Library, || {
        store.playback_favorite_page(&q).unwrap()
    });
    assert_eq!(next.total, None);
    assert_eq!(next.items.len(), 2);
    assert!(next_work.statements > 0);
    assert_eq!(head_work.statements, next_work.statements + 1);
}
