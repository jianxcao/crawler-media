//! Task 18 Step 1 (plan:404-410): unrelated-library growth, deep cursor sweeps, ordering
//! (NULL/tie/ASC/DESC/Unicode) and watch/count behaviour at the `library_item_page` seam.
//!
//! Rows are seeded with raw SQL because these assertions measure read work and page order;
//! the publish path itself is covered by the root lifecycle test and `scan_windows.rs`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use rusqlite::Connection;
use rusqlite::params;
use store::library_query::ItemPage;

use super::*;

/// Items in the measured library of the growth test.
const TARGET_ITEMS: usize = 300;
/// Items in the unrelated library before it grows.
const OTHER_START: usize = 1_000;
/// Items in the ordering/tie/Unicode library.
const ORDERING_ITEMS: usize = 240;
/// Items both shared-media libraries have (identical media ids).
const SHARED_ITEMS: usize = 200;
/// Items only the second shared-media library has.
const SECOND_ONLY_ITEMS: usize = 20;
const GROWTH_ENV: &str = "CRAWLER_MEDIA_SEEK_GROWTH_ITEMS";
const DEEP_ENV: &str = "CRAWLER_MEDIA_SEEK_DEEP_ITEMS";

fn env_count(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

/// One seeded `library_items` row plus the `media`/`library_files` rows it needs.
struct SeedItem {
    key: usize,
    title: String,
    title_sort: String,
    year: Option<i64>,
    date_sort: String,
    rating: f64,
    runtime: i64,
    added_at: i64,
    size: i64,
}

impl SeedItem {
    /// A deterministic movie. `title_sort` mirrors `title` so page order is observable.
    fn movie(key: usize, title: String) -> Self {
        Self {
            key,
            title_sort: title.clone(),
            title,
            year: Some(2000 + (key % 20) as i64),
            date_sort: format!("{:04}-01-01", 1950 + key % 70),
            rating: (key % 100) as f64 / 10.0,
            runtime: 60 + (key % 120) as i64,
            added_at: key as i64,
            size: 1_000 + key as i64,
        }
    }
}

/// A library plus the raw-seed identity of the rows seeded into it.
struct LibSeed {
    library_id: String,
    root_id: String,
    salt: u32,
    dir: String,
}

impl LibSeed {
    /// Create the library through the Store API (canonical rows), then read back its root.
    fn create(store: &Store, title: &str, dir: &str, salt: u32) -> Self {
        let library = store
            .create_library(MediaKind::Movie, title, &[dir], "everyone", true, &[])
            .unwrap();
        let root_id = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|row| row.library_id.as_deref() == Some(library.id.as_str()))
            .unwrap()
            .id;
        Self {
            library_id: library.id,
            root_id,
            salt,
            dir: dir.to_string(),
        }
    }

    /// Media id of a seeded key. Two libraries that share a salt share media ids.
    fn media_id(&self, key: usize) -> String {
        format!("{:08x}-0000-0000-0000-{key:012x}", self.salt)
    }

    /// File id of a seeded key; `file_salt` keeps a shared media id's files distinct.
    fn file_id(&self, file_salt: u32, key: usize) -> String {
        format!("{:08x}-0001-{file_salt:04x}-0000-{key:012x}", self.salt)
    }
}

/// Bulk-insert one library's items on a second connection; returns the media ids.
fn insert_items(db_path: &Path, lib: &LibSeed, file_salt: u32, items: &[SeedItem]) -> Vec<String> {
    let db = Connection::open(db_path).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        let media_id = lib.media_id(item.key);
        let file_id = lib.file_id(file_salt, item.key);
        let path = format!("{}/m{:07}.mkv", lib.dir, item.key);
        let shuffle = format!("{:020}", item.key);
        tx.execute(
            "INSERT OR IGNORE INTO media (media_id, kind, title, year, identity_state, \
             aliases_display_json, metadata_revision) \
             VALUES (?1,'movie',?2,?3,'confirmed','[]',0)",
            params![media_id, item.title, item.year],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO library_files (file_id, library_id, root_id, media_id, kind, path_key, \
             display_path, presence_state, publication_state, file_revision, source_version, \
             stat_known, size_bytes, quality_source, confidence) \
             VALUES (?1,?2,?3,?4,'movie',?5,?5,'present','published',1,'v1',1,?6,'probe','high')",
            params![
                file_id,
                lib.library_id,
                lib.root_id,
                media_id,
                path,
                item.size
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO library_items (library_id, media_id, kind, title, title_sort, year, \
             identity_state, metadata_revision, item_revision, release_date_sort, rating_sort, \
             runtime_sort, language, added_at_ns, added_at_sort, size_bytes, size_sort, \
             resolution_rank, scraped, stable_shuffle_key, file_count, available_file_count, \
             preferred_file_id) \
             VALUES (?1,?2,'movie',?3,?4,?5,'confirmed',0,1,?6,?7,?8,'en',?9,?9,?10,?10,3,1,?11,1,1,?12)",
            params![
                lib.library_id,
                media_id,
                item.title,
                item.title_sort,
                item.year,
                item.date_sort,
                item.rating,
                item.runtime,
                item.added_at,
                item.size,
                shuffle,
                file_id
            ],
        )
        .unwrap();
        ids.push(media_id);
    }
    tx.commit().unwrap();
    ids
}

/// One `user_library_items` row for a seeded key.
struct WatchRow {
    key: usize,
    seen: bool,
    played: bool,
    favorite: bool,
    last_played_at: i64,
}

fn insert_watch_rows(db_path: &Path, lib: &LibSeed, user_id: &str, rows: &[WatchRow]) {
    let db = Connection::open(db_path).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for row in rows {
        tx.execute(
            "INSERT INTO user_library_items (user_id, library_id, media_id, seen, played, \
             favorite, last_played_at, summary_revision) VALUES (?1,?2,?3,?4,?5,?6,?7,0)",
            params![
                user_id,
                lib.library_id,
                lib.media_id(row.key),
                row.seen as i64,
                row.played as i64,
                row.favorite as i64,
                row.last_played_at
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn query_for(
    lib: &LibSeed,
    user_id: Option<&str>,
    order: BrowseOrder,
    descending: bool,
    page: BrowsePage,
) -> LibraryQuery {
    LibraryQuery {
        library_id: lib.library_id.parse().unwrap(),
        user_id: user_id.map(|raw| raw.parse().unwrap()),
        filter: BrowseFilter::default(),
        order,
        descending,
        page,
        include_total: false,
    }
}

fn filtered_query(
    lib: &LibSeed,
    user_id: Option<&str>,
    order: BrowseOrder,
    descending: bool,
    page: BrowsePage,
    filter: BrowseFilter,
) -> LibraryQuery {
    let mut query = query_for(lib, user_id, order, descending, page);
    query.filter = filter;
    query
}

fn page_ids(page: &ItemPage) -> Vec<String> {
    page.items
        .iter()
        .map(|item| item.media_id.to_string())
        .collect()
}

/// Ids of one ordered offset read; `start` is the page jump, `limit` the page size.
fn offset_ids(store: &Store, base: &LibraryQuery, start: usize, limit: usize) -> Vec<String> {
    let mut query = base.clone();
    query.page = BrowsePage::Offset { start, limit };
    page_ids(&store.library_item_page(&query).unwrap())
}

/// Result of draining `BrowsePage::Cursor`: ids in returned order plus per-page lengths.
struct Sweep {
    ids: Vec<String>,
    page_lens: Vec<usize>,
}

fn cursor_sweep(store: &Store, base: &LibraryQuery, limit: usize) -> Sweep {
    let mut sweep = Sweep {
        ids: Vec::new(),
        page_lens: Vec::new(),
    };
    let mut after = None;
    loop {
        let mut query = base.clone();
        query.page = BrowsePage::Cursor {
            after: after.clone(),
            limit,
        };
        let page = store.library_item_page(&query).unwrap();
        sweep.page_lens.push(page.items.len());
        sweep.ids.extend(page_ids(&page));
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => return sweep,
        }
    }
}

/// Titles with heavy ties, case mixes and non-ASCII, so collation shows up in the order.
fn ordering_seed(total: usize) -> Vec<SeedItem> {
    const TITLES: [&str; 10] = [
        "alpha",
        "Alpha",
        "same",
        "same",
        "same",
        "Zebra",
        "Émile",
        "中文条目",
        "ünïcode",
        "Ångström",
    ];
    (0..total)
        .map(|key| {
            let mut item = SeedItem::movie(key, TITLES[key % TITLES.len()].to_string());
            item.title_sort = item.title.clone();
            if key % 7 == 0 {
                item.year = None;
            }
            item.date_sort = if key % 5 == 0 {
                String::new()
            } else {
                format!("{:04}-06-15", 1960 + key % 60)
            };
            item.rating = if key % 3 == 0 { 7.5 } else { (key % 10) as f64 };
            item.runtime = if key % 4 == 0 {
                90
            } else {
                60 + (key % 60) as i64
            };
            item.added_at = if key % 6 == 0 {
                1_700_000_000
            } else {
                key as i64
            };
            item.size = if key % 8 == 0 {
                4_096
            } else {
                1_000 + key as i64
            };
            item
        })
        .collect()
}

/// Half the ordering library has a watch row; the rest have none (NULL watch columns).
fn ordering_watch_rows(total: usize) -> Vec<WatchRow> {
    (0..total)
        .filter(|key| key % 2 == 0)
        .map(|key| WatchRow {
            key,
            seen: key % 4 != 0,
            played: key % 8 == 0,
            favorite: key % 6 == 0,
            last_played_at: if key % 10 == 0 { 0 } else { key as i64 * 1_000 },
        })
        .collect()
}

/// Watch rows for the dedup/count test: `key % 10 == 9` gets no row at all.
fn shared_watch_rows(total: usize) -> Vec<WatchRow> {
    (0..total)
        .filter(|key| key % 10 != 9)
        .map(|key| WatchRow {
            key,
            seen: key % 2 == 0,
            played: key % 6 == 0,
            favorite: key % 5 == 0,
            last_played_at: key as i64 * 7,
        })
        .collect()
}

fn watch_filter(watch: Option<&str>, unwatched_first: bool) -> BrowseFilter {
    BrowseFilter {
        watch: watch.map(str::to_string),
        unwatched_first,
        ..BrowseFilter::default()
    }
}

#[test]
fn seek_work_stays_flat_while_an_unrelated_library_grows() {
    let growth = env_count(GROWTH_ENV, 10_000);
    assert!(
        growth > OTHER_START,
        "{GROWTH_ENV} must exceed {OTHER_START}"
    );
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let db_path = data_dir.join("library.db");

    let target = LibSeed::create(&store, "Seek target", "/target", 1);
    let target_items: Vec<SeedItem> = (0..TARGET_ITEMS)
        .map(|key| SeedItem::movie(key, format!("Target {key:05}")))
        .collect();
    insert_items(&db_path, &target, 1, &target_items);

    let other = LibSeed::create(&store, "Other", "/other", 2);
    let cold: Vec<SeedItem> = (0..OTHER_START)
        .map(|key| SeedItem::movie(key, format!("Other {key:05}")))
        .collect();
    insert_items(&db_path, &other, 1, &cold);

    let query = query_for(
        &target,
        None,
        BrowseOrder::Title,
        false,
        BrowsePage::Offset {
            start: 100,
            limit: 50,
        },
    );
    let (before, before_stats) = store.library_item_page_with_stats(&query).unwrap();

    let grown: Vec<SeedItem> = (OTHER_START..growth)
        .map(|key| SeedItem::movie(key, format!("Other {key:05}")))
        .collect();
    let seed_start = Instant::now();
    insert_items(&db_path, &other, 1, &grown);
    let seed_secs = seed_start.elapsed().as_secs_f64();

    let (after, after_stats) = store.library_item_page_with_stats(&query).unwrap();
    let db_bytes = std::fs::metadata(&db_path)
        .map(|meta| meta.len())
        .unwrap_or_default();
    println!(
        "growth: other {OTHER_START}->{growth}, target {TARGET_ITEMS}; seed {seed_secs:.2}s; \
         db {db_bytes} B; sqlite {}; before {before_stats:?}; after {after_stats:?}",
        rusqlite::version()
    );

    assert_eq!(
        page_ids(&after),
        page_ids(&before),
        "page output changed while an unrelated library grew"
    );
    assert_eq!(after_stats.rows_returned, before_stats.rows_returned);
    assert_eq!(
        after_stats.fullscan_steps, 0,
        "the plain seek fell back to a full scan"
    );
    assert!(
        after_stats.vm_steps <= before_stats.vm_steps * 2,
        "vm_steps {} -> {} exceeds 2x the baseline",
        before_stats.vm_steps,
        after_stats.vm_steps
    );
}

#[test]
fn deep_cursor_sweep_has_no_duplicate_or_gap() {
    let total = env_count(DEEP_ENV, 50_000);
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let db_path = data_dir.join("library.db");
    let lib = LibSeed::create(&store, "Deep", "/deep", 3);
    let items: Vec<SeedItem> = (0..total)
        .map(|key| SeedItem::movie(key, format!("Deep {key:06}")))
        .collect();

    let seed_start = Instant::now();
    let seeded = insert_items(&db_path, &lib, 1, &items);
    let seed_secs = seed_start.elapsed().as_secs_f64();
    assert_eq!(seeded.len(), total);

    let base = query_for(
        &lib,
        None,
        BrowseOrder::Title,
        false,
        BrowsePage::Cursor {
            after: None,
            limit: 500,
        },
    );
    let sweep_start = Instant::now();
    let sweep = cursor_sweep(&store, &base, 500);
    let sweep_secs = sweep_start.elapsed().as_secs_f64();
    let one_shot = offset_ids(&store, &base, 0, total);
    println!(
        "deep: {total} items seeded in {seed_secs:.2}s; {} cursor pages in {sweep_secs:.2}s",
        sweep.page_lens.len()
    );

    assert_eq!(
        sweep.ids.len(),
        total,
        "cursor sweep returned the wrong count"
    );
    assert_eq!(
        sweep.ids, one_shot,
        "cursor order differs from a one-shot ordered read"
    );
    assert_eq!(
        sweep.ids.iter().collect::<BTreeSet<_>>().len(),
        total,
        "cursor sweep returned duplicates"
    );
    assert_eq!(
        sweep.ids.iter().cloned().collect::<BTreeSet<_>>(),
        seeded.iter().cloned().collect::<BTreeSet<_>>(),
        "cursor sweep missed or invented media ids"
    );
    assert_eq!(sweep.page_lens.len(), total.div_ceil(500));
    assert!(
        sweep.page_lens[..sweep.page_lens.len() - 1]
            .iter()
            .all(|len| *len == 500),
        "a non-final cursor page was short"
    );
    if total >= 10 {
        let jump = total - 10;
        assert_eq!(
            offset_ids(&store, &base, jump, 10),
            sweep.ids[jump..jump + 10],
            "an offset jump diverged from the cursor order"
        );
    }
}

#[test]
fn ordering_and_cursor_agree_across_null_ties_and_unicode() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let db_path = data_dir.join("library.db");
    let lib = LibSeed::create(&store, "Ordering", "/ordering", 4);
    let total = ORDERING_ITEMS;
    let items = ordering_seed(total);
    insert_items(&db_path, &lib, 1, &items);
    let user = "00000000-0000-0000-0010-000000000001";
    insert_watch_rows(&db_path, &lib, user, &ordering_watch_rows(total));

    let orders = [
        BrowseOrder::Title,
        BrowseOrder::ReleaseDate,
        BrowseOrder::Rating,
        BrowseOrder::Runtime,
        BrowseOrder::AddedAt,
        BrowseOrder::Size,
        BrowseOrder::LastPlayed,
        BrowseOrder::Random,
    ];
    for order in orders {
        for descending in [false, true] {
            let base = query_for(
                &lib,
                Some(user),
                order,
                descending,
                BrowsePage::Offset {
                    start: 0,
                    limit: total,
                },
            );
            let one_shot = offset_ids(&store, &base, 0, total);
            let sweep = cursor_sweep(&store, &base, 7);
            let label = format!("{order:?} descending={descending}");
            assert_eq!(sweep.ids, one_shot, "cursor diverged for {label}");
            assert_eq!(sweep.ids.len(), total, "short page sequence for {label}");
            assert_eq!(
                sweep.ids.iter().cloned().collect::<BTreeSet<_>>().len(),
                total,
                "duplicate row for {label}"
            );
            assert_eq!(sweep.page_lens.len(), total.div_ceil(7));
            assert!(
                sweep.page_lens[..sweep.page_lens.len() - 1]
                    .iter()
                    .all(|len| *len == 7),
                "a non-final cursor page was short for {label}"
            );
        }
    }
}

/// Expected media-id sets for the `watch` filter vocabulary.
///
/// The store maps `played | watched` to `played = 1` and `seen` to `seen = 1` (see
/// `library_query/items/filters.rs::watch_clause`), so `watched` is an alias of `played`,
/// never of `seen`. The fixture asserts both explicitly instead of assuming.
struct WatchSets {
    unwatched: BTreeSet<String>,
    seen: BTreeSet<String>,
    played: BTreeSet<String>,
    watching: BTreeSet<String>,
    favorite: BTreeSet<String>,
}

fn watch_sets(lib: &LibSeed, total: usize) -> WatchSets {
    let keys = |pred: fn(usize) -> bool| -> BTreeSet<String> {
        (0..total)
            .filter(|key| pred(*key))
            .map(|key| lib.media_id(key))
            .collect()
    };
    let sets = WatchSets {
        unwatched: keys(|key| key % 2 == 1),
        seen: keys(|key| key % 2 == 0),
        played: keys(|key| key % 6 == 0),
        watching: keys(|key| key % 2 == 0 && key % 6 != 0),
        favorite: keys(|key| key % 5 == 0),
    };
    assert_eq!(
        (
            sets.unwatched.len(),
            sets.seen.len(),
            sets.played.len(),
            sets.watching.len(),
            sets.favorite.len()
        ),
        (100, 100, 34, 66, 40),
        "watch fixture formula drifted"
    );
    sets
}

/// Every `watch` value the HTTP layer can pass through selects exactly its set, and
/// `library_item_count` agrees with the ids the cursor drain returns.
fn assert_watch_filters(store: &Store, lib: &LibSeed, user: &str, sets: &WatchSets) {
    let cases: [(&str, &BTreeSet<String>); 7] = [
        ("unwatched", &sets.unwatched),
        ("unseen", &sets.unwatched),
        ("seen", &sets.seen),
        ("played", &sets.played),
        ("watched", &sets.played),
        ("watching", &sets.watching),
        ("favorite", &sets.favorite),
    ];
    for (name, expected) in cases {
        let query = filtered_query(
            lib,
            Some(user),
            BrowseOrder::AddedAt,
            false,
            BrowsePage::Cursor {
                after: None,
                limit: 500,
            },
            watch_filter(Some(name), false),
        );
        let sweep = cursor_sweep(store, &query, 500);
        assert_eq!(
            sweep.ids.iter().cloned().collect::<BTreeSet<_>>(),
            *expected,
            "watch={name} returned the wrong set"
        );
        assert_eq!(
            store.library_item_count(&query).unwrap(),
            expected.len() as u64,
            "library_item_count disagrees for watch={name}"
        );
    }
}

/// `unwatched_first` groups watch tiers ahead of the requested order: unseen, then
/// seen-but-unplayed, then played, each still ascending by `added_at`.
fn tiered_order(lib: &LibSeed, sets: &WatchSets, total: usize) -> Vec<String> {
    let mut expected = Vec::with_capacity(total);
    for key in 0..total {
        if !sets.seen.contains(&lib.media_id(key)) {
            expected.push(lib.media_id(key));
        }
    }
    for key in 0..total {
        let id = lib.media_id(key);
        if sets.seen.contains(&id) && !sets.played.contains(&id) {
            expected.push(id);
        }
    }
    for key in 0..total {
        let id = lib.media_id(key);
        if sets.played.contains(&id) {
            expected.push(id);
        }
    }
    expected
}

/// The same media id seeded in two libraries is one card per library, each card pointing
/// at its own published file (never the other library's), and counts stay per-library.
fn assert_cross_library_cards(store: &Store, lib_a: &LibSeed, lib_b: &LibSeed, total: usize) {
    for (lib, file_salt, expected_total) in
        [(lib_a, 1, total), (lib_b, 2, total + SECOND_ONLY_ITEMS)]
    {
        let count_query = query_for(
            lib,
            None,
            BrowseOrder::Title,
            false,
            BrowsePage::Offset {
                start: 0,
                limit: 10,
            },
        );
        assert_eq!(
            store.library_item_count(&count_query).unwrap(),
            expected_total as u64,
            "library {} card count",
            lib.library_id
        );
        let all = query_for(
            lib,
            None,
            BrowseOrder::Title,
            false,
            BrowsePage::Offset {
                start: 0,
                limit: total,
            },
        );
        assert_eq!(offset_ids(store, &all, 0, total).len(), total);
        let first = store
            .library_item_page(&query_for(
                lib,
                None,
                BrowseOrder::Title,
                false,
                BrowsePage::Offset { start: 0, limit: 1 },
            ))
            .unwrap()
            .items
            .first()
            .cloned()
            .unwrap();
        assert_eq!(first.file_count, 1);
        assert_eq!(
            first.preferred_file_id,
            Some(lib.file_id(file_salt, 0).parse::<LedgerId>().unwrap())
        );
    }
}

#[test]
fn watch_filters_counts_and_cross_library_media_share_one_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let db_path = data_dir.join("library.db");
    let lib_a = LibSeed::create(&store, "Shared A", "/shared-a", 5);
    let lib_b = LibSeed::create(&store, "Shared B", "/shared-b", 5);
    let total = SHARED_ITEMS;

    let a_items: Vec<SeedItem> = (0..total)
        .map(|key| SeedItem::movie(key, format!("Shared {key:05}")))
        .collect();
    insert_items(&db_path, &lib_a, 1, &a_items);
    let b_items: Vec<SeedItem> = (0..total + SECOND_ONLY_ITEMS)
        .map(|key| SeedItem::movie(key, format!("Shared {key:05}")))
        .collect();
    insert_items(&db_path, &lib_b, 2, &b_items);

    let user = "00000000-0000-0000-0010-000000000002";
    insert_watch_rows(&db_path, &lib_a, user, &shared_watch_rows(total));
    let sets = watch_sets(&lib_a, total);
    assert_watch_filters(&store, &lib_a, user, &sets);

    // One snapshot: total, filtered count, cursor drain and offset page must agree.
    let mut counted = filtered_query(
        &lib_a,
        Some(user),
        BrowseOrder::Title,
        false,
        BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        watch_filter(Some("unwatched"), false),
    );
    counted.include_total = true;
    let (page, _) = store.library_item_page_with_stats(&counted).unwrap();
    assert_eq!(page.total, Some(sets.unwatched.len() as u64));
    assert_eq!(page.items.len(), 10);
    assert_eq!(
        store.library_item_count(&counted).unwrap(),
        sets.unwatched.len() as u64
    );
    assert_eq!(
        cursor_sweep(&store, &counted, 500).ids.len(),
        sets.unwatched.len()
    );

    // `unwatched_first` groups watch tiers before the requested order.
    let tiered = filtered_query(
        &lib_a,
        Some(user),
        BrowseOrder::AddedAt,
        false,
        BrowsePage::Offset {
            start: 0,
            limit: total,
        },
        watch_filter(None, true),
    );
    assert_eq!(
        offset_ids(&store, &tiered, 0, total),
        tiered_order(&lib_a, &sets, total)
    );

    assert_cross_library_cards(&store, &lib_a, &lib_b, total);
}

/// O01: a deep cursor in one large library costs about the same SQL work as the first
/// page, for both directions and the mixed watch-tier order (tuple seek, not OR chains).
#[test]
fn deep_cursor_work_matches_first_page_work() {
    let total = env_count(DEEP_ENV, 50_000);
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let db_path = data_dir.join("library.db");
    let lib = LibSeed::create(&store, "DeepWork", "/deep-work", 4);
    let items: Vec<SeedItem> = (0..total)
        .map(|key| {
            // Pairs of identical titles exercise the media_id tie-break.
            let mut item = SeedItem::movie(key, format!("Movie {:06}", key / 2 * 2));
            item.title_sort = item.title.clone();
            item
        })
        .collect();
    insert_items(&db_path, &lib, 1, &items);

    for descending in [false, true] {
        let first_query = query_for(
            &lib,
            None,
            BrowseOrder::Title,
            descending,
            BrowsePage::Cursor {
                after: None,
                limit: 50,
            },
        );
        let (first, first_stats) = store.library_item_page_with_stats(&first_query).unwrap();
        assert_eq!(first.items.len(), 50);
        // Walk to the middle in 500-row cursor pages (the page cap).
        let mut cursor = None;
        for _ in 0..total / 2 / 500 {
            let step = query_for(
                &lib,
                None,
                BrowseOrder::Title,
                descending,
                BrowsePage::Cursor {
                    after: cursor.clone(),
                    limit: 500,
                },
            );
            cursor = store.library_item_page(&step).unwrap().next_cursor;
        }
        let cursor = cursor.unwrap();
        let deep_query = query_for(
            &lib,
            None,
            BrowseOrder::Title,
            descending,
            BrowsePage::Cursor {
                after: Some(cursor),
                limit: 50,
            },
        );
        let (deep, deep_stats) = store.library_item_page_with_stats(&deep_query).unwrap();
        let expected = offset_ids(&store, &first_query, total / 2 / 500 * 500, 50);
        println!(
            "O01 desc={descending}: first vm={} deep vm={}",
            first_stats.vm_steps, deep_stats.vm_steps
        );
        assert_eq!(deep.items.len(), 50);
        assert_eq!(
            page_ids(&deep),
            expected,
            "deep page order (desc={descending})"
        );
        assert_eq!(deep_stats.fullscan_steps, 0);
        assert!(
            deep_stats.vm_steps <= first_stats.vm_steps * 2,
            "deep cursor work {} exceeds 2x first page {} (desc={descending})",
            deep_stats.vm_steps,
            first_stats.vm_steps
        );
    }
}
