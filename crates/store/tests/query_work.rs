//! Public read contracts and complete request work, with isolated synthetic facts.
use domain::{MediaKind, ProtocolParent};
use store::{
    Store,
    library_query::{BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery},
};

#[path = "query_work/support.rs"]
mod support;
use support::{protocol, seed};

#[test]
fn movie_request_work_does_not_grow_with_unrelated_tv() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    seed(&store, MediaKind::Movie, 12);
    let q = protocol();
    let (before, bw) = store.protocol_item_page_with_stats(&q).unwrap();
    seed(&store, MediaKind::Tv, 2000);
    let (after, aw) = store.protocol_item_page_with_stats(&q).unwrap();
    assert_eq!(before, after);
    println!("kind: before={bw:?} after={aw:?}");
    assert!(
        aw.vm_steps < bw.vm_steps + 2000,
        "unrelated kind should not enlarge the row set: {bw:?} -> {aw:?}"
    );
}
#[test]
fn item_work_includes_exact_count_and_auxiliary_queries() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = seed(&store, MediaKind::Movie, 400);
    let mut q = LibraryQuery {
        library_id: lib,
        user_id: None,
        filter: BrowseFilter {
            genres: vec!["Drama".into()],
            ..Default::default()
        },
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: false,
    };
    let (small, sw) = store.library_item_page_with_stats(&q).unwrap();
    q.include_total = true;
    let (counted, cw) = store.library_item_page_with_stats(&q).unwrap();
    assert_eq!(small.items, counted.items);
    assert_eq!(counted.total, Some(200));
    assert!(
        cw.vm_steps > sw.vm_steps * 2,
        "exact count must be included: {sw:?} -> {cw:?}"
    );
    assert_eq!(cw.statements, sw.statements + 1);
    let (_, again) = store.library_item_page_with_stats(&q).unwrap();
    assert_eq!(cw, again, "cached counters are per invocation");
}

#[test]
fn filtered_facets_use_one_snapshot_and_keep_exclude_own_dimension_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = seed(&store, MediaKind::Movie, 400);
    let request = FacetRequest {
        library_id: lib,
        user_id: None,
        filter: BrowseFilter {
            genres: vec!["Drama".into()],
            decades: vec!["1990s".into()],
            ..Default::default()
        },
        tier: FacetTier::All,
    };
    let (facets, work) = store.library_facets_with_stats(&request).unwrap();
    assert_eq!(facets.total, 0);
    assert!(facets.groups["genres"].iter().all(|v| v.count == 0));
    assert_eq!(
        facets.groups["decades"]
            .iter()
            .find(|v| v.value == "2020s")
            .unwrap()
            .count,
        200
    );
    assert_eq!(facets.groups["decades"].len(), 5);
    assert!(!facets.groups["watch"].iter().any(|v| v.value == "favorite"));
    println!("facets: {work:?}");
    assert_eq!(
        work.statements, 1,
        "all dimensions and total use the same statement snapshot"
    );
}

#[test]
fn instrumentation_is_isolated_across_threads_and_cleans_up_errors() {
    let workers: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                let dir = tempfile::tempdir().unwrap();
                let store = Store::open(dir.path()).unwrap();
                let lib = seed(&store, MediaKind::Movie, 40);
                let mut q = LibraryQuery {
                    library_id: lib,
                    user_id: None,
                    filter: BrowseFilter::default(),
                    order: BrowseOrder::Title,
                    descending: false,
                    page: BrowsePage::Offset {
                        start: 0,
                        limit: 10,
                    },
                    include_total: true,
                };
                let (plain, work) = store.library_item_page_with_stats(&q).unwrap();
                q.filter.decades = vec!["invalid".into()];
                assert!(store.library_item_page_with_stats(&q).is_err());
                q.filter.decades.clear();
                let (again, after) = store.library_item_page_with_stats(&q).unwrap();
                assert_eq!(plain, again);
                assert_eq!(work, after);
                assert_eq!(store.library_item_page(&q).unwrap(), plain);
                work
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results[0], results[1]);
}

#[test]
fn named_protocol_genre_does_not_resolve_names_by_scanning_matching_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = seed(&store, MediaKind::Movie, 100);
    let db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    db.execute("INSERT INTO library_item_terms(library_id,media_id,term_kind,term_value)
        SELECT library_id,media_id,'jellyfin_genre','name:drama' FROM library_item_terms WHERE term_kind='genre' AND term_value='Drama'",[]).unwrap();
    let genre_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"Drama")
        .simple()
        .to_string();
    db.execute("INSERT INTO library_item_terms(library_id,media_id,term_kind,term_value)
        SELECT library_id,media_id,'jellyfin_genre',?1 FROM library_item_terms WHERE term_kind='genre' AND term_value='Drama'",
        [format!("id:{genre_id}")]).unwrap();
    let mut query = protocol();
    query.parent = ProtocolParent::Library(lib);
    query.genres = vec!["Drama".into()];
    let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(page.total, 50);
    for genre in [
        " Drama ".to_string(),
        format!(
            "GENRE_{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"Drama").simple()
        ),
    ] {
        query.genres = vec![genre.clone()];
        assert_eq!(
            store.protocol_item_page(&query).unwrap().total,
            50,
            "genre {genre}"
        );
    }
    query.genres.clear();
    let (_, plain) = store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(
        work.statements, plain.statements,
        "plain genre names need no request-time name resolution query"
    );
}
