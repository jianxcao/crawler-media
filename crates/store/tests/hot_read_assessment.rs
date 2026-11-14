//! Observational Task9 assessment; exact query contracts remain unchanged.
use domain::{LibraryId, MediaKind, ProtocolParent, UserId};
use std::time::Instant;
use store::{
    Store,
    library_query::{
        BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery,
        items::QueryWorkStats,
    },
    stats::StatsScope,
};
#[path = "query_work/support.rs"]
mod support;

fn sample(label: &str, expected: u64, mut query: impl FnMut() -> (u64, QueryWorkStats)) {
    let mut times = Vec::new();
    let mut work = None;
    for _ in 0..20 {
        let start = Instant::now();
        let (total, stats) = query();
        times.push(start.elapsed().as_secs_f64() * 1000.);
        assert_eq!(total, expected, "{label}");
        work = Some(stats);
    }
    times.sort_by(f64::total_cmp);
    eprintln!(
        "ASSESS {label} total={expected} samples=20 p50_ms={:.3} p95_ms={:.3} max_ms={:.3} work={:?}",
        times[9],
        times[18],
        times[19],
        work.unwrap()
    );
}
fn overlapping_library(store: &Store, first: LibraryId) {
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Overlap",
            &["/assessment-overlap"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == lib.id)
        .unwrap()
        .id;
    let conn = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    conn.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) SELECT printf('%08x-0000-0000-0002-000000000000',rowid),?1,?2,media_id,'movie','/assessment-overlap/'||file_id,'/assessment-overlap/'||file_id,'present','published',1,'v1','probe','high' FROM library_files WHERE library_id=?3",rusqlite::params![lib.id,root,first.to_string()]).unwrap();
    conn.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) SELECT file_id,library_id,media_id,-1,-1 FROM library_files WHERE library_id=?1",[&lib.id]).unwrap();
    let op = "assessment-overlap-repair";
    store
        .begin_library_integrity(lib.id.parse().unwrap(), true, op, 2000)
        .unwrap();
    loop {
        let state = store.advance_library_operation(op, 2001, 100).unwrap();
        if state.state != "applying" {
            assert_eq!(state.state, "succeeded");
            break;
        }
    }
}
fn assess(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = support::seed(&store, MediaKind::Movie, size);
    assess_browse(&store, lib, size);
    assess_protocol(&store, lib, size);
}
fn assess_browse(store: &Store, lib: LibraryId, size: usize) {
    let mut browse = LibraryQuery {
        library_id: lib,
        user_id: Some(UserId::new()),
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 100,
        },
        include_total: true,
    };
    browse.filter.unwatched_first = true;
    sample(
        &format!("P2_unwatched_first_size{size}"),
        size as u64,
        || {
            let (p, w) = store.library_item_page_with_stats(&browse).unwrap();
            assert_eq!(p.items.len(), 100);
            (p.total.unwrap(), w)
        },
    );
    browse.filter.unwatched_first = false;
    sample(&format!("P3_index_size{size}"), size as u64, || {
        let (p, w) = store.with_stats(StatsScope::Library, || store.library_item_index(&browse));
        (p.unwrap().iter().map(|b| b.count).sum(), w)
    });
    let unfiltered = FacetRequest {
        library_id: lib,
        user_id: browse.user_id,
        filter: BrowseFilter::default(),
        tier: FacetTier::All,
    };
    sample(
        &format!("P8_facets_no_filter_size{size}"),
        size as u64,
        || {
            let (p, w) = store.library_facets_with_stats(&unfiltered).unwrap();
            (p.total, w)
        },
    );
    let facets = FacetRequest {
        library_id: lib,
        user_id: browse.user_id,
        filter: BrowseFilter {
            genres: vec!["Drama".into()],
            identity: Some("provisional".into()),
            ..Default::default()
        },
        tier: FacetTier::All,
    };
    sample(
        &format!("P8_facets_residual_size{size}"),
        size.div_ceil(2) as u64,
        || {
            let (p, w) = store.library_facets_with_stats(&facets).unwrap();
            (p.total, w)
        },
    );
    browse.filter = facets.filter.clone();
    for total in [false, true] {
        browse.include_total = total;
        sample(
            &format!("P9_residual_include_total{total}_size{size}"),
            size.div_ceil(2) as u64,
            || {
                let (p, w) = store.library_item_page_with_stats(&browse).unwrap();
                assert_eq!(p.items.len(), 100);
                if total {
                    assert_eq!(p.total, Some(size.div_ceil(2) as u64));
                } else {
                    assert!(p.total.is_none());
                }
                (size.div_ceil(2) as u64, w)
            },
        );
    }
}
fn assess_protocol(store: &Store, lib: LibraryId, size: usize) {
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(lib);
    query.limit = 100;
    query.offset = size - 100;
    sample(
        &format!("P7_protocol_deep_offset_size{size}"),
        size as u64,
        || {
            let (p, w) = store.protocol_item_page_with_stats(&query).unwrap();
            assert_eq!(p.items.len(), 100);
            (p.total, w)
        },
    );
    query.offset = 0;
    for term in ["t", "em"] {
        query.search_term = Some(term.into());
        sample(
            &format!("P10_middle_substring_{term}_size{size}"),
            size as u64,
            || {
                let (p, w) = store.protocol_item_page_with_stats(&query).unwrap();
                assert_eq!(p.items.len(), 100);
                (p.total, w)
            },
        );
    }
    query.search_term = None;
    query.parent = ProtocolParent::Root;
    overlapping_library(store, lib);
    sample(
        &format!("P6_root_two_full_overlap_size{size}"),
        size as u64,
        || {
            let (p, w) = store.protocol_item_page_with_stats(&query).unwrap();
            assert_eq!(p.items.len(), 100);
            let ids: std::collections::HashSet<_> = p.items.iter().map(|r| &r.item).collect();
            assert_eq!(ids.len(), 100);
            (p.total, w)
        },
    );
}
#[test]
#[ignore = "manual observational Task9 assessment; no universal SLO assertion"]
fn remaining_hot_read_work_and_latency_at_1k_and_10k() {
    for size in [1000, 10000] {
        assess(size);
    }
}
