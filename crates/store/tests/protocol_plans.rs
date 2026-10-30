//! Protocol reads must bound work before hydrating the returned page.
use domain::{
    LibraryId, Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType, ProtocolParent,
    UnitKey, User, UserId, UserRole,
};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file_units};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    user: UserId,
    library: LibraryId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let user = UserId::new();
        store
            .insert_user(&User {
                id: user,
                login: "protocol-plans".into(),
                enabled: true,
                role: UserRole::Admin,
            })
            .unwrap();
        let library = store.default_library(MediaKind::Tv).unwrap().unwrap();
        Self {
            _dir: dir,
            store,
            user,
            library: library.id.parse().unwrap(),
        }
    }

    fn series(&self, name: &str, season: i32, episodes: i32) -> MediaId {
        let id = MediaId::new();
        self.store
            .insert_media(&Media {
                id,
                kind: MediaKind::Tv,
                title: name.into(),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        let root = self
            .store
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap()
            .root_paths[0]
            .clone();
        let units: Vec<_> = (1..=episodes)
            .map(|episode| UnitKey::new(season, episode))
            .collect();
        publish_fixture_file_units(
            &self.store,
            &FixtureFile {
                media_id: id,
                path: root.join(format!("{id}.mkv")).display().to_string(),
                ..Default::default()
            },
            &units,
        )
        .unwrap();
        id
    }

    fn mark(&self, media: MediaId, unit: UnitKey, played: bool, favorite: bool) {
        self.store
            .upsert_unit(
                self.user,
                media,
                unit.season,
                unit.episode,
                0,
                Some(played),
                Some(favorite),
                None,
                None,
                None,
                false,
                100,
            )
            .unwrap();
    }

    fn query(&self) -> ProtocolItemQuery {
        ProtocolItemQuery {
            user_id: self.user,
            parent: ProtocolParent::Library(self.library),
            series_id: None,
            season: None,
            person_ids: vec![],
            studio_ids: vec![],
            search_term: None,
            genres: vec![],
            genre_ids: vec![],
            filters: vec![],
            is_favorite: None,
            is_played: None,
            include_types: vec![ProtocolItemType::Series],
            recursive: true,
            is_folder: None,
            sort: vec![],
            offset: 0,
            limit: 20,
        }
    }
}

#[test]
fn episodes_of_pinned_series_do_not_skip_scan() {
    let f = Fixture::new();
    let media = f.series("Selected series", 100, 8);
    let mut query = f.query();
    query.parent = ProtocolParent::Series(media);
    query.include_types = vec![ProtocolItemType::Episode];
    query.limit = 3;
    let (before, small) = f.store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(before.total, 8);
    assert_eq!(before.items.len(), 3);
    for index in 0..40 {
        f.series(&format!("Unrelated {index}"), 1, 30);
    }
    let (after, large) = f.store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(after.total, before.total);
    assert_eq!(after.items, before.items);
    assert!(
        large.vm_steps <= small.vm_steps + 1200,
        "pinned page must seek selected Media; unrelated units inflated work: {small:?} -> {large:?}"
    );
    assert!(large.fullscan_steps <= small.fullscan_steps + 8);
}

#[test]
fn played_filter_count_expands_cte_once() {
    let measure = |episodes| {
        let f = Fixture::new();
        let media = f.series("Completed series", 1, episodes);
        f.mark(media, UnitKey::WHOLE, true, false);
        let mut query = f.query();
        query.is_played = Some(true);
        query.limit = 0;
        let (page, stats) = f.store.protocol_item_page_with_stats(&query).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.items.is_empty());
        stats
    };
    let small = measure(20);
    let large = measure(800);
    assert!(
        large.vm_steps <= small.vm_steps + 1000,
        "work-level played count must read the work summary once, independent of episode count: {small:?} -> {large:?}"
    );
}

#[test]
fn played_and_favorite_counts_match_page_totals() {
    let f = Fixture::new();
    let all = f.series("All played and favorite", 1, 3);
    let partial = f.series("Partially played", 1, 3);
    let unseen = f.series("Unseen favorite", 1, 2);
    f.mark(all, UnitKey::WHOLE, true, true);
    f.mark(partial, UnitKey::new(1, 1), true, true);
    f.mark(unseen, UnitKey::WHOLE, false, true);
    for (types, totals) in [
        (vec![ProtocolItemType::Series], [1, 2, 2, 1]),
        (vec![ProtocolItemType::Episode], [4, 4, 1, 0]),
        (
            vec![ProtocolItemType::Series, ProtocolItemType::Episode],
            [5, 6, 3, 1],
        ),
    ] {
        for ((played, favorite), expected) in [
            (Some(true), None),
            (Some(false), None),
            (None, Some(true)),
            (Some(false), Some(true)),
        ]
        .into_iter()
        .zip(totals)
        {
            let mut query = f.query();
            query.include_types = types.clone();
            query.is_played = played;
            query.is_favorite = favorite;
            let complete = f.store.protocol_item_page(&query).unwrap();
            assert_eq!(
                complete.total, expected,
                "types={types:?}, played={played:?}, favorite={favorite:?}"
            );
            assert_eq!(complete.items.len() as u64, expected);
            query.limit = 1;
            let mut actual = Vec::new();
            loop {
                let page = f.store.protocol_item_page(&query).unwrap();
                assert_eq!(page.total, expected);
                if page.items.is_empty() {
                    break;
                }
                actual.extend(page.items);
                query.offset += 1;
            }
            assert_eq!(actual, complete.items);
            assert_eq!(actual.len() as u64, expected);
        }
    }
}
