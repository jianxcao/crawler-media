//! Manual release matrix: synthetic facts, production Store queries in a temporary data directory.
//! PERF_SIZES=5000,20000,50000,100000 cargo test --release -p store --test query_work_perf -- --ignored --nocapture
use domain::{LibraryId, MediaKind, ProtocolParent};
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

fn enrich(store: &Store, lib: LibraryId) {
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO library_item_terms(library_id,media_id,term_kind,term_value)
        SELECT library_id,media_id,'genre','All' FROM library_items WHERE library_id=?1",
        [lib.to_string()],
    )
    .unwrap();
    let rows:Vec<(String,String)>=tx.prepare("SELECT media_id,term_value FROM library_item_terms WHERE library_id=?1 AND term_kind='genre'").unwrap()
        .query_map([lib.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    for (media, genre) in rows {
        let wire = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, genre.as_bytes())
            .simple()
            .to_string();
        for key in [
            format!("name:{}", genre.to_lowercase()),
            format!("id:{wire}"),
        ] {
            tx.execute("INSERT INTO library_item_terms(library_id,media_id,term_kind,term_value) VALUES(?1,?2,'jellyfin_genre',?3)",rusqlite::params![lib.to_string(),media,key]).unwrap();
        }
    }
    tx.commit().unwrap();
}
fn measure(label: &str, expected: u64, mut query: impl FnMut() -> (u64, QueryWorkStats)) {
    let mut durations = Vec::new();
    let mut last = QueryWorkStats::default();
    for _ in 0..7 {
        let start = Instant::now();
        let (total, stats) = query();
        durations.push(start.elapsed().as_secs_f64() * 1000.);
        assert_eq!(total, expected, "{label}");
        last = stats;
    }
    durations.sort_by(f64::total_cmp);
    println!(
        "{label}\ttotal={expected}\tmedian={:.3}\tmax={:.3}\t{last:?}",
        durations[3], durations[6]
    );
}
fn run(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = support::seed(&store, MediaKind::Movie, size);
    enrich(&store, lib);
    println!("size={size} SQLite={}", rusqlite::version());
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
    let movie = support::seed(&store, MediaKind::Movie, 1000);
    support::seed(&store, MediaKind::Tv, size);
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
#[test]
fn synthetic_query_shapes_preserve_exact_totals() {
    run(24);
}
