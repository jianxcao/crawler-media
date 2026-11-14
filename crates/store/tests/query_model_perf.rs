//! Release read-model matrix: raw NFO facts repaired through the public Integrity operation.
//! PERF_SIZES=5000,20000,50000,100000 cargo test --release -p store --test query_model_perf -- --ignored --nocapture
use domain::{LibraryId, MediaKind, ProtocolParent, ProtocolSortKey};
use std::time::Instant;
use store::{
    Store,
    library_query::{
        BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery,
        items::QueryWorkStats,
    },
};
#[path = "query_work/support.rs"]
mod support;

fn fixed_library(store: &Store, kind: MediaKind) -> LibraryId {
    let id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("query-library-{}", kind.as_str()).as_bytes(),
    )
    .to_string();
    let root_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("query-root-{}", kind.as_str()).as_bytes(),
    )
    .to_string();
    let path = format!("/query-work/{}", kind.as_str());
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute("INSERT INTO libraries(library_id,kind,name,is_default,access_mode,admin_visible) VALUES(?1,?2,'Work',0,'everyone',1)",rusqlite::params![id,kind.as_str()]).unwrap();
    db.execute(
        "INSERT INTO library_roots(root_id,library_id,path_key,display_path) VALUES(?1,?2,?3,?3)",
        rusqlite::params![root_id, id, path],
    )
    .unwrap();
    id.parse().unwrap()
}

fn expected_media(kind: MediaKind, index: usize) -> domain::MediaId {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("query-media-{}-{index}", kind.as_str()).as_bytes(),
    )
    .to_string()
    .parse()
    .unwrap()
}

fn measure(label: &str, expected: u64, mut query: impl FnMut() -> (u64, QueryWorkStats)) {
    let first_start = Instant::now();
    let (first_total, _) = query();
    let first_ms = first_start.elapsed().as_secs_f64() * 1_000.;
    assert_eq!(first_total, expected, "{label} first");
    for _ in 0..5 {
        assert_eq!(query().0, expected, "{label} warmup");
    }
    let mut durations = Vec::with_capacity(50);
    let mut work = None;
    for _ in 0..50 {
        let start = Instant::now();
        let (total, stats) = query();
        durations.push(start.elapsed().as_secs_f64() * 1000.);
        assert_eq!(total, expected, "{label}");
        if let Some(previous) = &work {
            let previous: &QueryWorkStats = previous;
            assert_eq!(stats.vm_steps, previous.vm_steps, "{label} VM drift");
            assert_eq!(
                stats.statements, previous.statements,
                "{label} statement drift"
            );
            assert_eq!(
                stats.sort_operations, previous.sort_operations,
                "{label} sort drift"
            );
            assert_eq!(
                stats.hydrated_rows, previous.hydrated_rows,
                "{label} hydration drift"
            );
        }
        work = Some(stats);
    }
    durations.sort_by(f64::total_cmp);
    println!(
        "{label}\ttotal={expected}\tfirst_ms={first_ms:.3}\tp50_ms={:.3}\tp95_ms={:.3}\tmax_ms={:.3}\t{:?}",
        durations[24],
        durations[47],
        durations[49],
        work.unwrap()
    );
}
fn run(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = fixed_library(&store, MediaKind::Movie);
    let repair_started = Instant::now();
    support::seed_into(&store, MediaKind::Movie, 0, size, lib, "/query-work/movie");
    let repair_ms = repair_started.elapsed().as_secs_f64() * 1_000.;
    let db_path = store.data_dir().join("library.db");
    let db_bytes = std::fs::metadata(&db_path).unwrap().len();
    let wal_bytes = std::fs::metadata(format!("{}-wal", db_path.display()))
        .map(|m| m.len())
        .ok();
    println!(
        "size={size} SQLite={} fixture_ms={repair_ms:.3} db_bytes={db_bytes} wal_bytes={wal_bytes:?} convention=first+5-warmup+50-timed-nearest-rank",
        rusqlite::version()
    );
    for (name, filter, expected) in [
        ("none", BrowseFilter::default(), size as u64),
        (
            "drama",
            BrowseFilter {
                genres: vec!["Drama".into()],
                ..Default::default()
            },
            size.div_ceil(2) as u64,
        ),
        (
            "all",
            BrowseFilter {
                genres: vec!["All".into()],
                ..Default::default()
            },
            size as u64,
        ),
        (
            "multi",
            BrowseFilter {
                genres: vec!["Drama".into()],
                decades: vec!["2020s".into()],
                stock: vec!["unscraped".into()],
                ..Default::default()
            },
            size.div_ceil(2) as u64,
        ),
    ] {
        let q = LibraryQuery {
            library_id: lib,
            user_id: None,
            filter: filter.clone(),
            order: BrowseOrder::Title,
            descending: false,
            page: BrowsePage::Offset {
                start: 0,
                limit: 100,
            },
            include_total: true,
        };
        measure(&format!("items_{name}"), expected, || {
            let (p, w) = store.library_item_page_with_stats(&q).unwrap();
            (p.total.unwrap(), w)
        });
        let r = FacetRequest {
            library_id: lib,
            user_id: None,
            filter,
            tier: FacetTier::All,
        };
        measure(&format!("facets_{name}"), expected, || {
            let (p, w) = store.library_facets_with_stats(&r).unwrap();
            (p.total, w)
        });
    }
    for (name, genres, total) in [
        ("movie", vec![], size as u64),
        ("drama", vec!["Drama".into()], size.div_ceil(2) as u64),
        ("all", vec!["All".into()], size as u64),
    ] {
        let mut q = support::protocol();
        q.parent = ProtocolParent::Library(lib);
        q.limit = 100;
        q.genres = genres;
        measure(&format!("protocol_{name}"), total, || {
            let (p, w) = store.protocol_item_page_with_stats(&q).unwrap();
            (p.total, w)
        });
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let movie = fixed_library(&store, MediaKind::Movie);
    let _tv = fixed_library(&store, MediaKind::Tv);
    support::seed_into(
        &store,
        MediaKind::Movie,
        0,
        1000,
        movie,
        "/query-work/movie",
    );
    support::seed_into(&store, MediaKind::Tv, 0, size, _tv, "/query-work/tv");
    for parent in [ProtocolParent::Library(movie), ProtocolParent::Root] {
        let mut q = support::protocol();
        q.parent = parent;
        q.limit = 100;
        measure(
            if matches!(q.parent, ProtocolParent::Root) {
                "kind_root"
            } else {
                "kind_library"
            },
            1000,
            || {
                let (p, w) = store.protocol_item_page_with_stats(&q).unwrap();
                (p.total, w)
            },
        );
    }
}
#[test]
#[ignore = "manual release measurement"]
fn query_matrix() {
    for size in std::env::var("PERF_SIZES")
        .unwrap_or_else(|_| "5000,20000,50000,100000".into())
        .split(',')
        .map(|v| v.parse().unwrap())
    {
        run(size);
    }
}

fn extended_query_matrix(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = fixed_library(&store, MediaKind::Movie);
    support::seed_into(&store, MediaKind::Movie, 0, size, lib, "/query-work/movie");
    let mut protocol = support::protocol();
    protocol.parent = ProtocolParent::Library(lib);
    protocol.limit = 100;
    protocol.sort[0].descending = true;
    assert_eq!(protocol.sort[0].key, ProtocolSortKey::Name);
    measure("protocol_name_desc", size as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(&protocol).unwrap();
        assert_eq!(page.items.len(), size.min(100));
        (page.total, work)
    });
    let mut browse = LibraryQuery {
        library_id: lib,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 100,
        },
        include_total: true,
    };
    measure("work_default_title_asc", size as u64, || {
        let (page, work) = store.library_item_page_with_stats(&browse).unwrap();
        assert_eq!(page.items.len(), size.min(100));
        (page.total.unwrap(), work)
    });
    browse.page = BrowsePage::Offset {
        start: size / 2,
        limit: 100,
    };
    measure("work_deep_offset", size as u64, || {
        let (page, work) = store.library_item_page_with_stats(&browse).unwrap();
        assert_eq!(page.items.len(), (size - size / 2).min(100));
        (page.total.unwrap(), work)
    });
    browse.page = BrowsePage::Offset { start: 0, limit: 0 };
    measure("work_limit_zero", size as u64, || {
        let (page, work) = store.library_item_page_with_stats(&browse).unwrap();
        assert!(page.items.is_empty());
        (page.total.unwrap(), work)
    });
    browse.page = BrowsePage::Offset {
        start: size,
        limit: 100,
    };
    measure("work_empty_page", size as u64, || {
        let (page, work) = store.library_item_page_with_stats(&browse).unwrap();
        assert!(page.items.is_empty());
        (page.total.unwrap(), work)
    });

    // Remove the NFO raw inputs, then use the public repair operation. This is
    // a separate true no-Metadata/no-genre shape, not the paired NFO fixture.
    let db_path = store.data_dir().join("library.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "DELETE FROM library_metadata_inputs WHERE library_id=?1",
        [lib.to_string()],
    )
    .unwrap();
    drop(db);
    let op = format!("query-empty-nfo-{}", uuid::Uuid::new_v4());
    let repair_started = Instant::now();
    store
        .begin_library_integrity(lib, true, &op, 2_000)
        .unwrap();
    loop {
        let state = store.advance_library_operation(&op, 2_001, 100).unwrap();
        if state.state != "applying" {
            assert_eq!(state.state, "succeeded");
            break;
        }
    }
    println!(
        "true_no_metadata size={size} repair_ms={:.3} db_bytes={} wal_bytes={:?}",
        repair_started.elapsed().as_secs_f64() * 1_000.,
        std::fs::metadata(&db_path).unwrap().len(),
        std::fs::metadata(format!("{}-wal", db_path.display()))
            .map(|m| m.len())
            .ok()
    );
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let nfo_rows: i64 = db
        .query_row("SELECT count(*) FROM library_metadata_inputs", [], |r| {
            r.get(0)
        })
        .unwrap();
    let adopted_rows: i64 = db
        .query_row("SELECT count(*) FROM media_metadata", [], |r| r.get(0))
        .unwrap();
    assert_eq!((nfo_rows, adopted_rows), (0, 0));
    drop(db);
    browse.page = BrowsePage::Offset {
        start: 0,
        limit: 100,
    };
    measure("true_no_metadata_work", size as u64, || {
        let (page, work) = store.library_item_page_with_stats(&browse).unwrap();
        assert!(page.items.iter().all(|item| !item.scraped));
        (page.total.unwrap(), work)
    });
    let facets = FacetRequest {
        library_id: lib,
        user_id: None,
        filter: BrowseFilter::default(),
        tier: FacetTier::All,
    };
    measure("true_no_metadata_facets", size as u64, || {
        let (page, work) = store.library_facets_with_stats(&facets).unwrap();
        assert!(page.groups["genres"].iter().all(|bucket| bucket.count == 0));
        (page.total, work)
    });
}

#[test]
#[ignore = "manual sequential release measurement"]
fn expanded_matrix() {
    let size = std::env::var("PERF_SIZE")
        .unwrap_or_else(|_| "100000".into())
        .parse()
        .unwrap();
    extended_query_matrix(size);
}
#[test]
fn synthetic_query_shapes_preserve_exact_totals() {
    run(24);
}

#[test]
fn fixture_oracle_matches_raw_nfo_facts_and_visible_ids() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = fixed_library(&store, MediaKind::Movie);
    support::seed_into(&store, MediaKind::Movie, 0, 24, lib, "/query-work/movie");
    for (genres, indices) in [
        (vec![], (0..24).collect::<Vec<_>>()),
        (vec!["Drama".into()], (0..24).step_by(2).collect()),
        (vec!["All".into()], (0..24).collect()),
    ] {
        let query = LibraryQuery {
            library_id: lib,
            user_id: None,
            filter: BrowseFilter {
                genres: genres.clone(),
                ..Default::default()
            },
            order: BrowseOrder::Title,
            descending: false,
            page: BrowsePage::Offset {
                start: 0,
                limit: 100,
            },
            include_total: true,
        };
        let expected: Vec<_> = indices
            .iter()
            .map(|i| expected_media(MediaKind::Movie, *i))
            .collect();
        let page = store.library_item_page(&query).unwrap();
        assert_eq!(page.total, Some(expected.len() as u64));
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.media_id)
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            page.items
                .iter()
                .all(|item| item.year == Some(2024) && !item.scraped)
        );
        let mut protocol = support::protocol();
        protocol.parent = ProtocolParent::Library(lib);
        protocol.limit = 100;
        protocol.genres = genres;
        let page = store.protocol_item_page(&protocol).unwrap();
        assert_eq!(page.total, expected.len() as u64);
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.item.media_id())
                .collect::<Vec<_>>(),
            expected
        );
    }
}
