//! The first-page plan diagnostic follows the observable browse contract.
use domain::{LibraryId, Media, MediaId, MediaKind};
use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};
use store::{
    Db, Store, StoreError,
    library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery},
    stats::StatsScope,
};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    library_id: LibraryId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let root = dir.path().join("movies");
        std::fs::create_dir(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap();
        for (title, year, resolution) in [("Zulu", 1999, "720p"), ("Alpha", 2024, "1080p")] {
            let media = Media {
                id: MediaId::new(),
                kind: MediaKind::Movie,
                title: title.into(),
                year: Some(year),
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            };
            store.insert_media(&media).unwrap();
            store::test_support::publish_fixture_file(
                &store,
                &store::test_support::FixtureFile {
                    media_id: media.id,
                    path: root.join(format!("{title}.mkv")).display().to_string(),
                    resolution: Some(resolution.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        Self {
            _dir: dir,
            store,
            library_id: library.id.parse().unwrap(),
        }
    }

    fn query(&self) -> LibraryQuery {
        LibraryQuery {
            library_id: self.library_id,
            user_id: None,
            filter: BrowseFilter::default(),
            order: BrowseOrder::Title,
            descending: false,
            page: BrowsePage::Cursor {
                after: None,
                limit: 1,
            },
            include_total: false,
        }
    }
}

#[test]
fn ordinary_first_page_plan_and_browse_match_observable_fixture() {
    let f = Fixture::new();
    let query = f.query();
    let before = f.store.library_item_page(&query).unwrap();
    assert_eq!(
        before
            .items
            .iter()
            .map(|i| i.title.as_str())
            .collect::<Vec<_>>(),
        ["Alpha"]
    );
    assert!(before.next_cursor.is_some());
    let plans = f.store.library_item_page_query_plans(&query).unwrap();
    assert!(!plans.is_empty());
    assert!(
        plans
            .iter()
            .any(|p| p.to_ascii_uppercase().contains("SEARCH")),
        "{plans:?}"
    );
    assert!(
        plans
            .iter()
            .all(|p| !p.to_ascii_uppercase().contains("TEMP B-TREE")),
        "{plans:?}"
    );
    assert_eq!(f.store.library_item_page(&query).unwrap(), before);

    let mut filtered = query;
    filtered.filter.decades.push("1990s".into());
    let page = f.store.library_item_page(&filtered).unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|i| i.title.as_str())
            .collect::<Vec<_>>(),
        ["Zulu"]
    );
    assert!(
        !f.store
            .library_item_page_query_plans(&filtered)
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.store.library_item_page(&filtered).unwrap(), page);
}

#[test]
fn unsupported_variants_fail_instead_of_explaining_another_query() {
    let f = Fixture::new();
    let first = f.query();
    let cursor = f
        .store
        .library_item_page(&first)
        .unwrap()
        .next_cursor
        .unwrap();
    let mut query = first.clone();
    query.page = BrowsePage::Cursor {
        after: Some(cursor),
        limit: 1,
    };
    assert!(matches!(
        f.store.library_item_page_query_plans(&query),
        Err(StoreError::Invalid(_))
    ));
    query = first.clone();
    query.page = BrowsePage::Offset { start: 0, limit: 1 };
    assert!(matches!(
        f.store.library_item_page_query_plans(&query),
        Err(StoreError::Invalid(_))
    ));
    query = first.clone();
    query.order = BrowseOrder::LastPlayed;
    assert!(matches!(
        f.store.library_item_page_query_plans(&query),
        Err(StoreError::Invalid(_))
    ));
    query = first.clone();
    query.include_total = true;
    assert!(matches!(
        f.store.library_item_page_query_plans(&query),
        Err(StoreError::Invalid(_))
    ));
    query = first;
    query.filter.decades.push("invalid".into());
    assert!(matches!(
        f.store.library_item_page(&query),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        f.store.library_item_page_query_plans(&query),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn plan_uses_reader_while_library_writer_is_held() {
    let f = Fixture::new();
    let held_store = Arc::clone(&f.store);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        held_store.with_writer(Db::Library, |_| {
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        })
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (plans, stats) = f.store.with_stats(StatsScope::Library, || {
        f.store.library_item_page_query_plans(&f.query())
    });
    release_tx.send(()).unwrap();
    writer.join().unwrap().unwrap();
    assert!(!plans.unwrap().is_empty());
    assert!(stats.reader_checkouts > 0);
    assert_eq!(stats.writer_hold_ns, 0);
}
