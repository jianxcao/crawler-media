//! Search pagination must avoid sorting every match twice to select one owner.
use domain::{LibraryId, Media, MediaId, MediaKind};
use store::Store;
use store::stats::StatsScope;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        Self { dir, store }
    }
    fn library(&self, name: &str) -> LibraryId {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.store
            .create_library(
                MediaKind::Movie,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap()
    }
    fn media(&self, title: &str) -> MediaId {
        let id = MediaId::new();
        self.store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: title.into(),
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
        id
    }
    fn publish(&self, library: LibraryId, media: MediaId, title: &str) {
        let config = self
            .store
            .get_library(&library.to_string())
            .unwrap()
            .unwrap();
        let path = config.root_paths[0].join(format!("{media}.mkv"));
        std::fs::write(&path, b"movie").unwrap();
        std::fs::write(
            path.with_extension("nfo"),
            format!("<movie><title>{title}</title></movie>"),
        )
        .unwrap();
        publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
}

#[test]
fn matching_three_thousand_media_need_at_most_one_page_order_sort() {
    let f = Fixture::new();
    let library = f.library("movies");
    let mut expected: Vec<_> = (0..3000)
        .map(|n| {
            let title = format!("Ab Beacon {n:04}");
            let media = f.media(&title);
            f.publish(library, media, &title);
            media
        })
        .collect();
    let other = f.library("alternate");
    let (lower, upper) = if library.to_string() < other.to_string() {
        (library, other)
    } else {
        (other, library)
    };
    let shared = f.media("Window-spanning shared identity");
    f.publish(lower, shared, "Ab Beacon Zzzz");
    f.publish(upper, shared, "Ab Beacon !Earlier");
    expected.push(shared);
    let scope = [library, other];
    let measurements: Vec<_> = ["beacon", "ab"]
        .into_iter()
        .map(|term| {
            let (first, stats) = f.store.with_stats(StatsScope::Library, || {
                f.store
                    .search_library_items(&scope, term, Some(50), None)
                    .unwrap()
            });
            assert_eq!(
                first
                    .items
                    .iter()
                    .map(|hit| hit.media_id)
                    .collect::<Vec<_>>(),
                expected[..50]
            );
            assert!(first.next_cursor.is_some());
            assert!(
                stats.statements > 0 && stats.vm_steps > 0,
                "inactive stats: {stats:?}"
            );
            eprintln!("3000 matching Media / page50 / {term}: {stats:?}");
            let mut all = first
                .items
                .iter()
                .map(|hit| hit.media_id)
                .collect::<Vec<_>>();
            let mut cursor = first.next_cursor;
            while let Some(after) = cursor {
                let page = f
                    .store
                    .search_library_items(&scope, term, Some(50), Some(&after))
                    .unwrap();
                assert_eq!(
                    page.items.len(),
                    (expected.len() - all.len()).min(50),
                    "no duplicate owner may consume a page slot"
                );
                all.extend(page.items.iter().map(|hit| hit.media_id));
                cursor = page.next_cursor;
            }
            assert_eq!(all, expected, "complete stable cursor traversal for {term}");
            (term, stats)
        })
        .collect();
    common_page_stays_bounded_past_visible_nonmatches(&f, library, &scope, &expected);
    let hidden = f.library("hidden-ordered-predecessors");
    for n in 0..1025 {
        let title = format!("Aa Nonmatching {n:04}");
        let media = f.media(&title);
        f.publish(hidden, media, &title);
    }
    // More than any fixed ordered fast window precedes the wanted matches.
    // Neither out-of-scope nor actively hidden raw rows may truncate results.
    for scope in [vec![library, other], vec![library, other, hidden]] {
        if scope.contains(&hidden) {
            f.store.begin_library_removal(&hidden.to_string()).unwrap();
        }
        for term in ["beacon", "ab"] {
            let page = f
                .store
                .search_library_items(&scope, term, Some(50), None)
                .unwrap();
            assert_eq!(
                page.items
                    .iter()
                    .map(|hit| hit.media_id)
                    .collect::<Vec<_>>(),
                expected[..50]
            );
            assert!(
                page.next_cursor.is_some(),
                "fallback must retain a complete page"
            );
        }
    }
    for (term, stats) in measurements {
        // At 3000 common matches, selection should inspect an ordered bounded
        // window instead of reducing all matched rows before its page limit.
        assert!(
            stats.vm_steps <= 40_000,
            "common-match page work for {term}: {stats:?}"
        );
        // A final top-N title order is allowed; an additional full-match owner
        // ranking sort is not.
        assert!(
            stats.sort_operations <= 1,
            "extra all-match owner sorting for {term}: {stats:?}"
        );
    }
}

fn common_page_stays_bounded_past_visible_nonmatches(
    f: &Fixture,
    library: LibraryId,
    scope: &[LibraryId],
    expected: &[MediaId],
) {
    for n in 0..320 {
        let title = format!("Aa Visible predecessor {n:04}");
        let media = f.media(&title);
        f.publish(library, media, &title);
    }
    let (page, stats) = f.store.with_stats(StatsScope::Library, || {
        f.store
            .search_library_items(scope, "beacon", Some(50), None)
            .unwrap()
    });
    assert_eq!(
        page.items
            .iter()
            .map(|hit| hit.media_id)
            .collect::<Vec<_>>(),
        expected[..50]
    );
    assert!(page.next_cursor.is_some());
    assert!(
        stats.statements > 0 && stats.vm_steps > 0,
        "inactive stats: {stats:?}"
    );
    eprintln!("common long-term search after 320 visible predecessors: {stats:?}");
    assert!(
        stats.vm_steps <= 40_000,
        "visible nonmatching predecessors must not trigger exhaustive common-match reduction: {stats:?}"
    );
}

#[test]
fn matching_visible_owner_is_chosen_before_cursor_even_when_titles_differ() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let (lower, upper) = if a.to_string() < b.to_string() {
        (a, b)
    } else {
        (b, a)
    };
    let shared = f.media("Shared identity");
    f.publish(lower, shared, "Ab Zebra Beacon");
    f.publish(upper, shared, "Ab Alpha Beacon");
    let middle = f.media("Ab Middle Beacon");
    f.publish(lower, middle, "Ab Middle Beacon");
    for term in ["beacon", "ab"] {
        let first = f
            .store
            .search_library_items(&[a, b], term, Some(1), None)
            .unwrap();
        assert_eq!(first.items[0].media_id, middle);
        let second = f
            .store
            .search_library_items(&[a, b], term, Some(1), first.next_cursor.as_deref())
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].media_id, shared);
        assert_eq!(second.items[0].library_id, lower);
        assert_eq!(second.items[0].title, "Ab Zebra Beacon");
        assert!(second.next_cursor.is_none());
    }
    let only_upper_matches = f
        .store
        .search_library_items(&[a, b], "alpha", Some(1), None)
        .unwrap();
    assert_eq!(only_upper_matches.items.len(), 1);
    assert_eq!(only_upper_matches.items[0].media_id, shared);
    assert_eq!(only_upper_matches.items[0].library_id, upper);
}
