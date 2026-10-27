use super::*;

#[test]
fn facets_count_each_dimension_without_its_own_condition() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let unfiltered = store
        .library_facets(&FacetRequest {
            library_id,
            user_id: None,
            filter: BrowseFilter::default(),
            tier: FacetTier::All,
        })
        .unwrap();
    assert_eq!(unfiltered.total, 3);
    let genres = unfiltered.groups.get("genres").expect("genres");
    assert_eq!(
        genres
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("18", 1), ("28", 1)]
    );
    assert_eq!(genres[0].label, "剧情");
    // Rating thresholds: 6 and 7 are satisfied by both rated items, 8 and 9 only by Alpha.
    assert_eq!(
        unfiltered
            .groups
            .get("ratings")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("6", 2), ("7", 2), ("8", 1), ("9", 1)]
    );
    // Fixed buckets are always returned, zero-count ones included, in the documented closed
    // set and semantic order (docs/api-contracts/self.md: 候选全集…含 count: 0).
    assert_eq!(
        unfiltered
            .groups
            .get("runtimes")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("lte60", 0), ("60to90", 0), ("90to120", 1), ("gt120", 1)]
    );
    assert_eq!(
        unfiltered
            .groups
            .get("decades")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [
            ("earlier", 1),
            ("1990s", 0),
            ("2000s", 0),
            ("2010s", 1),
            ("2020s", 1)
        ]
    );
    assert_eq!(
        unfiltered
            .groups
            .get("stock")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("missing", 1), ("unscraped", 1)]
    );
    assert_eq!(
        unfiltered
            .groups
            .get("hdr")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("true", 1), ("false", 2)]
    );

    // With a genre filter the genre dimension ignores its own condition: both genres keep
    // their library-wide counts, while every other dimension is restricted by the filter.
    let filtered = store
        .library_facets(&FacetRequest {
            library_id,
            user_id: None,
            filter: BrowseFilter {
                genres: vec!["28".into()],
                ..BrowseFilter::default()
            },
            tier: FacetTier::All,
        })
        .unwrap();
    assert_eq!(filtered.total, 1);
    assert_eq!(
        filtered
            .groups
            .get("genres")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("18", 1), ("28", 1)]
    );
    assert_eq!(
        filtered
            .groups
            .get("decades")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [
            ("earlier", 0),
            ("1990s", 0),
            ("2000s", 0),
            ("2010s", 0),
            ("2020s", 1)
        ]
    );

    // A filter nobody satisfies keeps the candidates but zeroes their counts; no watch
    // candidate is fabricated when nothing is favourite.
    let empty = store
        .library_facets(&FacetRequest {
            library_id,
            user_id: None,
            filter: BrowseFilter {
                watch: Some("favorite".into()),
                ..BrowseFilter::default()
            },
            tier: FacetTier::All,
        })
        .unwrap();
    assert_eq!(empty.total, 0);
    assert_eq!(
        empty
            .groups
            .get("genres")
            .unwrap()
            .iter()
            .map(|count| count.count)
            .collect::<Vec<_>>(),
        [0, 0]
    );
    assert_eq!(
        empty
            .groups
            .get("watch")
            .unwrap()
            .iter()
            .map(|count| (count.value.as_str(), count.count))
            .collect::<Vec<_>>(),
        [("unwatched", 3), ("watching", 0), ("played", 0)]
    );

    // `primary` fills only the four panel dimensions.
    let primary = store
        .library_facets(&FacetRequest {
            library_id,
            user_id: None,
            filter: BrowseFilter::default(),
            tier: FacetTier::Primary,
        })
        .unwrap();
    assert_eq!(primary.groups.len(), 4);
    assert!(primary.groups.contains_key("genres"));
    assert!(!primary.groups.contains_key("stock"));
}

#[test]
fn item_index_buckets_follow_the_browse_order() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();
    let titles = store
        .library_item_titles(&query(
            library_id,
            BrowseFilter::default(),
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(titles, ["Alpha", "Bravo", "Charlie"]);

    // Descending order yields the reverse letter order, so buckets are not title-sorted.
    let descending = store
        .library_item_titles(&query(
            library_id,
            BrowseFilter::default(),
            BrowseOrder::Title,
            true,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(descending, ["Charlie", "Bravo", "Alpha"]);
}

#[test]
fn item_facts_carry_seasons_and_studio_per_page() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let show = create_media(&store, ALPHA, "Show", MediaKind::Tv, Some(2024), Some("9"));
    adopt(
        &store,
        show.id,
        "en-US",
        work_fields(
            "Show",
            Some(8.0),
            Some(50),
            Some("en"),
            &[("10759", "Action & Adventure")],
        ),
    );
    publish(
        &store,
        &scope,
        vec![
            (
                make_file(
                    &library,
                    &root,
                    show.id,
                    FileShape {
                        file_id: "00000000-0000-0000-0002-000000000001",
                        path: "Show/S01E01.mkv",
                        resolution: Some("1080p"),
                        hdr: None,
                        presence: FilePresence::Present,
                    },
                ),
                vec![UnitKey::new(1, 1)],
            ),
            (
                make_file(
                    &library,
                    &root,
                    show.id,
                    FileShape {
                        file_id: "00000000-0000-0000-0002-000000000002",
                        path: "Show/S02E01.mkv",
                        resolution: Some("1080p"),
                        hdr: None,
                        presence: FilePresence::Present,
                    },
                ),
                vec![UnitKey::new(2, 1)],
            ),
        ],
    );
    let library_id: LibraryId = library.id.parse().unwrap();
    let page = store
        .library_item_page(&query(
            library_id,
            BrowseFilter::default(),
            BrowseOrder::Title,
            false,
            offset(0, 10),
        ))
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].distinct_episode_count, 2);
    assert_eq!(page.items[0].distinct_season_count, 2);

    let facts = store
        .library_item_facts(library_id, &[show.id], None)
        .unwrap();
    let facts = facts.get(&show.id).expect("facts for the page");
    assert_eq!(facts.seasons, [1, 2]);

    // The episode list owns what the published files cover, from `library_units`.
    let episodes = store
        .library_episodes(library_id, show.id, Some(1), "")
        .unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!((episodes[0].season, episodes[0].episode), (1, 1));
    assert!(episodes[0].owned);
    assert_eq!(episodes[0].file_ids.len(), 1);
    // Season 2 was never adopted as metadata, so only the ownership row is known.
    let season_two = store
        .library_episodes(library_id, show.id, Some(2), "")
        .unwrap();
    assert_eq!(season_two.len(), 1);
    assert!(season_two[0].owned);
    // Season 2 has no adopted metadata: the title comes from the published file's
    // name fallback, and nothing is fabricated for fields only a metadata source
    // can provide.
    assert!(
        season_two[0]
            .title
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty()),
        "an owned episode without metadata falls back to its file name"
    );
    assert!(season_two[0].overview.is_none());
    assert!(season_two[0].air_date.is_none());
    assert!(
        store
            .library_episodes(library_id, show.id, Some(3), "")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn detail_returns_stored_columns_and_file_coverage() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id: LibraryId = library.id.parse().unwrap();

    let detail = store
        .library_item_detail(library_id, ALPHA.parse().unwrap(), "en-US")
        .unwrap()
        .expect("item detail");
    assert_eq!(detail.item.title, "Alpha");
    assert_eq!(detail.item.year, Some(2024));
    assert_eq!(detail.item.rating, Some(9.0));
    assert_eq!(detail.item.language.as_deref(), Some("en"));
    assert_eq!(detail.item.resolution_rank, 4);
    assert_eq!(detail.item.max_resolution(), "2160p");
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.file_units.len(), 1);
    assert_eq!(detail.file_units[0].1, [UnitKey::WHOLE]);
    assert_eq!(detail.item.file_count, 1);

    // Metadata comes from the adopted row even when the requested locale is not the locale
    // that has a row: the store falls back through the library locale to any row.
    assert_eq!(detail.metadata.title.value(), Some(&"Alpha".to_string()));
    assert_eq!(detail.metadata.rating.value(), Some(&9.0));

    // An unknown item is absent, not empty.
    assert!(
        store
            .library_item_detail(
                library_id,
                "00000000-0000-0000-0000-0000000000ff".parse().unwrap(),
                "en-US"
            )
            .unwrap()
            .is_none()
    );
}
