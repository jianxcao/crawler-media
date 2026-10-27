//! B2: authorized favorite pages are selected in SQL from playback units and
//! library items, never from a whole-ledger scan.

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
    offset: usize,
) -> PlaybackViewQuery {
    PlaybackViewQuery {
        user,
        libraries,
        limit,
        offset,
        order: FavoriteOrder::FavoritedAt,
        descending: true,
        unwatched_first: false,
    }
}

#[test]
fn favorite_page_is_authorized_deduplicated_and_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (ra, rb) = (tmp.path().join("a"), tmp.path().join("b"));
    let a = library(&store, &ra, "A");
    let b = library(&store, &rb, "B");
    let user = UserId::new();
    let episode_fav = show(&store, "Episode Fav");
    episode(&store, episode_fav, &ra, 1);
    episode(&store, episode_fav, &ra, 2);
    let shared = show(&store, "Shared");
    episode(&store, shared, &ra, 1);
    episode(&store, shared, &rb, 2);
    let hidden = show(&store, "Hidden Only");
    episode(&store, hidden, &rb, 1);
    let unfavored = show(&store, "Not Favorite");
    episode(&store, unfavored, &ra, 1);
    favorite(&store, user, episode_fav, 1, 2, 3_000);
    favorite(&store, user, shared, -1, -1, 2_000);
    favorite(&store, user, hidden, 1, 1, 4_000);

    let page = store
        .playback_favorite_page(&query(user, vec![a], 50, 0))
        .unwrap();
    let ids: Vec<MediaId> = page.items.iter().map(|r| r.media_id).collect();
    assert_eq!(ids, vec![episode_fav, shared]);
    assert_eq!(page.total, 2);
    assert!(page.items.iter().all(|r| r.library_id == a));
    assert_eq!(
        (page.items[0].unit.season, page.items[0].unit.episode),
        (1, 2)
    );
    assert!(page.items[1].unit.is_whole());

    let both = store
        .playback_favorite_page(&query(user, vec![a, b], 50, 0))
        .unwrap();
    assert_eq!(
        both.total, 3,
        "shared Media is counted once across libraries"
    );
    let mut paged = Vec::new();
    for offset in 0..both.total {
        let one = store
            .playback_favorite_page(&query(user, vec![a, b], 1, offset))
            .unwrap();
        assert_eq!(one.total, both.total);
        paged.extend(one.items.into_iter().map(|r| r.media_id));
    }
    assert_eq!(paged, vec![hidden, episode_fav, shared]);
    assert_eq!(
        store
            .playback_favorite_page(&query(user, vec![], 50, 0))
            .unwrap()
            .total,
        0
    );
}
