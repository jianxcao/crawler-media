//! Library browse contract: SQL filters, keyset paging, facet counts, page facts and
//! detail/episode reads — all against the new `library_*` summaries.

use domain::{
    Confidence, EntityScope, FieldValue, FilePresence, FilePublication, FileVersion, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, MetadataAdoption, MetadataFields, MetadataGenre,
    MetadataPreferences, NormalizedMetadata, QualitySource, UnitKey, UserId,
};
use store::library_query::{
    BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::Store;

fn create_media(
    store: &Store,
    id: &str,
    title: &str,
    kind: MediaKind,
    year: Option<u16>,
    external_id: Option<&str>,
) -> Media {
    let media = Media {
        id: id.parse().unwrap(),
        kind,
        title: title.to_string(),
        year,
        original_title: None,
        tmdb_id: external_id.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    media
}

fn create_library(store: &Store, name: &str, kind: MediaKind, root: &str) -> (store::Library, String) {
    let library = store
        .create_library(kind, name, &[root], "everyone", true, &[])
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id == Some(library.id.clone()))
        .expect("library root");
    (library, root.id)
}

struct FileShape<'a> {
    file_id: &'a str,
    path: &'a str,
    resolution: Option<&'a str>,
    hdr: Option<&'a str>,
    presence: FilePresence,
}

fn make_file(
    lib: &store::Library,
    root_id: &str,
    media_id: MediaId,
    shape: FileShape<'_>,
) -> LibraryFile {
    LibraryFile {
        file_id: shape.file_id.parse().unwrap(),
        library_id: lib.id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id,
        kind: lib.kind,
        path: shape.path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: shape.presence,
        publication: FilePublication::Published,
        size_bytes: Some(1_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash".to_string()),
        source_path: None,
        transfer_mode: None,
        resolution: shape.resolution.map(str::to_string),
        codec: Some("hevc".to_string()),
        hdr: shape.hdr.map(str::to_string),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(120_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn publish(store: &Store, scope: &WriteScope, files: Vec<(LibraryFile, Vec<UnitKey>)>) {
    let publications = files
        .iter()
        .map(|(file, units)| StorePublication {
            file,
            units,
            expected_version: None,
            probe_required: false,
        })
        .collect::<Vec<_>>();
    store.publish_files(scope, &publications, 1000).unwrap();
}

fn work_fields(
    title: &str,
    rating: Option<f64>,
    runtime: Option<u32>,
    language: Option<&str>,
    genres: &[(&str, &str)],
) -> MetadataFields {
    MetadataFields {
        title: FieldValue::Value(title.to_string()),
        rating: rating.map_or(FieldValue::Missing, FieldValue::Value),
        runtime_minutes: runtime.map_or(FieldValue::Missing, FieldValue::Value),
        language: language.map_or(FieldValue::Missing, |value| FieldValue::Value(value.to_string())),
        genres: FieldValue::Value(
            genres
                .iter()
                .map(|(filter_key, name)| MetadataGenre {
                    source: "tmdb".into(),
                    source_id: Some((*filter_key).to_string()),
                    name: (*name).to_string(),
                    filter_key: (*filter_key).to_string(),
                })
                .collect(),
        ),
        ..MetadataFields::default()
    }
}

/// Adopt metadata before publishing: the reducer reads the adopted row when it builds the
/// `library_items` summary, exactly like the real pipeline does.
fn adopt(store: &Store, media_id: MediaId, locale: &str, fields: MetadataFields) {
    store
        .adopt_metadata(
            &MetadataAdoption {
                media_id,
                scope: EntityScope::Work,
                locale: locale.to_string(),
                source: "tmdb".into(),
                provider_entity_id: "1".into(),
                provider_work_id: "1".into(),
                fetched_at: 1000,
                normalized: NormalizedMetadata {
                    fields,
                    field_origins: std::collections::BTreeMap::new(),
                },
                expected_revision: None,
                preferences: MetadataPreferences {
                    language_priority: vec![locale.to_string()],
                    cert_country_priority: vec!["US".into()],
                },
            },
            1000,
        )
        .unwrap();
}

fn query(
    library_id: LibraryId,
    filter: BrowseFilter,
    order: BrowseOrder,
    descending: bool,
    page: BrowsePage,
) -> LibraryQuery {
    LibraryQuery {
        library_id,
        user_id: None,
        filter,
        order,
        descending,
        page,
        include_total: true,
    }
}

fn offset(start: usize, limit: usize) -> BrowsePage {
    BrowsePage::Offset { start, limit }
}

const ALPHA: &str = "00000000-0000-0000-0000-0000000000a1";
const BRAVO: &str = "00000000-0000-0000-0000-0000000000b2";
const CHARLIE: &str = "00000000-0000-0000-0000-0000000000c3";

/// Three movies: Alpha (2024, rating 9, 130 min, en, genre 28, 2160p HDR),
/// Bravo (2015, rating 7, 100 min, ja, genre 18, 1080p), Charlie (1985, no metadata, 720p).
fn seeded() -> (tempfile::TempDir, Store, store::Library, WriteScope) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = create_library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let alpha = create_media(&store, ALPHA, "Alpha", MediaKind::Movie, Some(2024), Some("1"));
    let bravo = create_media(&store, BRAVO, "Bravo", MediaKind::Movie, Some(2015), Some("2"));
    let charlie = create_media(&store, CHARLIE, "Charlie", MediaKind::Movie, Some(1985), None);
    adopt(&store, alpha.id, "en-US", work_fields("Alpha", Some(9.0), Some(130), Some("en"), &[("28", "Action")]));
    adopt(&store, bravo.id, "en-US", work_fields("Bravo", Some(7.0), Some(100), Some("ja"), &[("18", "Drama")]));
    publish(
        &store,
        &scope,
        vec![
            (
                make_file(&library, &root, alpha.id, FileShape {
                    file_id: "00000000-0000-0000-0001-0000000000a1",
                    path: "Alpha.mkv",
                    resolution: Some("2160p"),
                    hdr: Some("HDR10"),
                    presence: FilePresence::Present,
                }),
                vec![UnitKey::WHOLE],
            ),
            (
                make_file(&library, &root, bravo.id, FileShape {
                    file_id: "00000000-0000-0000-0001-0000000000b2",
                    path: "Bravo.mkv",
                    resolution: Some("1080p"),
                    hdr: None,
                    presence: FilePresence::Present,
                }),
                vec![UnitKey::WHOLE],
            ),
            (
                make_file(&library, &root, charlie.id, FileShape {
                    file_id: "00000000-0000-0000-0001-0000000000c3",
                    path: "Charlie.mkv",
                    resolution: Some("720p"),
                    hdr: None,
                    presence: FilePresence::Missing,
                }),
                vec![UnitKey::WHOLE],
            ),
        ],
    );
    (tmp, store, library, scope)
}

#[test]
fn filters_restrict_items_and_total_agrees() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let all = store
        .library_item_page(&query(library_id, BrowseFilter::default(), BrowseOrder::Title, false, offset(0, 50)))
        .unwrap();
    assert_eq!(all.total, Some(3));
    assert_eq!(all.items.len(), 3);

    // rating_gte over NULL-rated items returns nothing (Charlie has no adopted rating).
    let rated = store
        .library_item_page(&query(
            library_id,
            BrowseFilter { min_rating: Some(9.0), ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(rated.total, Some(1));
    assert_eq!(rated.items.iter().map(|item| item.title.as_str()).collect::<Vec<_>>(), ["Alpha"]);

    // rating_gte=9 with a 9.5 cut still finds Alpha; 9.6 finds nobody.
    let strict = store
        .library_item_page(&query(
            library_id,
            BrowseFilter { min_rating: Some(9.6), ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(strict.total, Some(0));
    assert!(strict.items.is_empty());

    // Every dimension restricts, and the page count always matches the total.
    let cases: Vec<(&str, BrowseFilter, Vec<&str>)> = vec![
        (
            "genre",
            BrowseFilter { genres: vec!["28".into()], ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "resolution",
            BrowseFilter { resolutions: vec!["720p".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "decade",
            BrowseFilter { decades: vec!["2020s".into()], ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "earlier",
            BrowseFilter { decades: vec!["earlier".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "2010s",
            BrowseFilter { decades: vec!["2010s".into()], ..BrowseFilter::default() },
            vec!["Bravo"],
        ),
        (
            "empty decade bucket",
            BrowseFilter { decades: vec!["1990s".into()], ..BrowseFilter::default() },
            vec![],
        ),
        (
            "runtime",
            BrowseFilter { runtimes: vec!["gt120".into()], ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "language",
            BrowseFilter { languages: vec!["JA".into()], ..BrowseFilter::default() },
            vec!["Bravo"],
        ),
        (
            "hdr",
            BrowseFilter { hdr: Some(true), ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "missing",
            BrowseFilter { stock: vec!["missing".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "unscraped",
            BrowseFilter { stock: vec!["unscraped".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "confirmed",
            BrowseFilter { identity: Some("confirmed".into()), ..BrowseFilter::default() },
            vec!["Alpha", "Bravo"],
        ),
        (
            "provisional",
            BrowseFilter { identity: Some("provisional".into()), ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
    ];
    for (name, filter, expected) in cases {
        let page = store
            .library_item_page(&query(library_id, filter, BrowseOrder::Title, false, offset(0, 50)))
            .unwrap();
        let titles = page.items.iter().map(|item| item.title.as_str()).collect::<Vec<_>>();
        assert_eq!(titles, expected, "dimension {name} returned {titles:?}");
        assert_eq!(page.total, Some(expected.len() as u64), "dimension {name} total");
    }

    // A filter that matches nothing still agrees between page and total.
    let none = store
        .library_item_page(&query(
            library_id,
            BrowseFilter { genres: vec!["999".into()], ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(none.total, Some(0));
    assert!(none.items.is_empty());
}

#[test]
fn invalid_filter_values_are_rejected() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();
    for filter in [
        BrowseFilter { decades: vec!["2030s".into()], ..BrowseFilter::default() },
        BrowseFilter { runtimes: vec!["lte0".into()], ..BrowseFilter::default() },
        BrowseFilter { stock: vec!["gone".into()], ..BrowseFilter::default() },
        BrowseFilter { watch: Some("binged".into()), ..BrowseFilter::default() },
        BrowseFilter { identity: Some("maybe".into()), ..BrowseFilter::default() },
        BrowseFilter { min_rating: Some(11.0), ..BrowseFilter::default() },
    ] {
        let error = store
            .library_item_page(&query(library_id, filter.clone(), BrowseOrder::Title, false, offset(0, 10)))
            .expect_err("filter must be rejected");
        assert!(matches!(error, store::StoreError::Invalid(_)), "got {error:?}");
    }
}

#[test]
fn cursor_pages_round_trip_without_gaps() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let offset_order = store
        .library_item_page(&query(library_id, BrowseFilter::default(), BrowseOrder::Title, false, offset(0, 50)))
        .unwrap()
        .items
        .iter()
        .map(|item| item.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(offset_order, ["Alpha", "Bravo", "Charlie"]);

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .library_item_page(&query(
                library_id,
                BrowseFilter::default(),
                BrowseOrder::Title,
                false,
                BrowsePage::Cursor { after: after.clone(), limit: 2 },
            ))
            .unwrap();
        for item in &page.items {
            seen.push(item.title.clone());
        }
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
        assert!(seen.len() <= 3, "cursor walk did not terminate");
    }
    assert_eq!(seen, offset_order);

    // The same walk in the other direction follows the requested direction exactly.
    let mut descending = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .library_item_page(&query(
                library_id,
                BrowseFilter::default(),
                BrowseOrder::Title,
                true,
                BrowsePage::Cursor { after, limit: 2 },
            ))
            .unwrap();
        descending.extend(page.items.iter().map(|item| item.title.clone()));
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    assert_eq!(descending, ["Charlie", "Bravo", "Alpha"]);

    // A cursor that does not parse, or a well-formed one with the wrong shape, is an error.
    for cursor in ["not json", "{\"keys\":[],\"media_id\":\"nope\"}", "{}"] {
        let error = store
            .library_item_page(&query(
                library_id,
                BrowseFilter::default(),
                BrowseOrder::Title,
                false,
                BrowsePage::Cursor { after: Some(cursor.to_string()), limit: 2 },
            ))
            .expect_err("cursor must be rejected");
        assert!(matches!(error, store::StoreError::Invalid(_)), "got {error:?}");
    }
}

#[test]
fn unwatched_includes_media_without_a_user_row() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let cases = [
        ("unwatched", 3usize),
        ("unseen", 3),
        ("played", 0),
        ("watching", 0),
        ("favorite", 0),
        ("seen", 0),
    ];
    for (watch, expected) in cases {
        let mut request = query(
            library_id,
            BrowseFilter { watch: Some(watch.into()), ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        );
        // Signed-in viewers with no `user_library_items` row behave exactly like signed-out
        // ones: the LEFT JOIN yields NULL and every watch predicate reads it as "not seen".
        request.user_id = Some(UserId::new());
        let page = store.library_item_page(&request).unwrap();
        assert_eq!(page.total, Some(expected as u64), "watch={watch}");
    }
}

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
            filter: BrowseFilter { genres: vec!["28".into()], ..BrowseFilter::default() },
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
            filter: BrowseFilter { watch: Some("favorite".into()), ..BrowseFilter::default() },
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
        [("unwatched", 0), ("watching", 0), ("played", 0)]
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
        work_fields("Show", Some(8.0), Some(50), Some("en"), &[("10759", "Action & Adventure")]),
    );
    publish(
        &store,
        &scope,
        vec![
            (
                make_file(&library, &root, show.id, FileShape {
                    file_id: "00000000-0000-0000-0002-000000000001",
                    path: "Show/S01E01.mkv",
                    resolution: Some("1080p"),
                    hdr: None,
                    presence: FilePresence::Present,
                }),
                vec![UnitKey::new(1, 1)],
            ),
            (
                make_file(&library, &root, show.id, FileShape {
                    file_id: "00000000-0000-0000-0002-000000000002",
                    path: "Show/S02E01.mkv",
                    resolution: Some("1080p"),
                    hdr: None,
                    presence: FilePresence::Present,
                }),
                vec![UnitKey::new(2, 1)],
            ),
        ],
    );
    let library_id: LibraryId = library.id.parse().unwrap();
    let page = store
        .library_item_page(&query(library_id, BrowseFilter::default(), BrowseOrder::Title, false, offset(0, 10)))
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
    assert!(store
        .library_episodes(library_id, show.id, Some(3), "")
        .unwrap()
        .is_empty());
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
    assert!(store
        .library_item_detail(library_id, "00000000-0000-0000-0000-0000000000ff".parse().unwrap(), "en-US")
        .unwrap()
        .is_none());
}
