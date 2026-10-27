//! Public read contracts and complete request work, with isolated synthetic facts.
use domain::{MediaKind, ProtocolParent, ProtocolSort, ProtocolSortKey};
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
    seed(&store, MediaKind::Movie, 1000);
    let mut q = protocol();
    q.genres = vec!["Drama".into()];
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
    assert!(cw.vm_steps > sw.vm_steps, "the count is measured");
    assert!(cw.statements > sw.statements);
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute("UPDATE library_files SET publication_state='pending' WHERE media_id IN (SELECT media_id FROM library_items WHERE library_id=?1 AND media_id IN (SELECT media_id FROM library_item_terms WHERE term_kind='genre' AND term_value='Drama') LIMIT 1)", [lib.to_string()]).unwrap();
    let op = "query-work-delete";
    store.begin_library_integrity(lib, true, op, 2000).unwrap();
    while store
        .advance_library_operation(op, 2001, 100)
        .unwrap()
        .state
        == "applying"
    {}
    let (after, work) = store.library_item_page_with_stats(&q).unwrap();
    assert_eq!(after.total, Some(199));
    assert!(work.vm_steps > 0 && work.statements > 0);
    assert_eq!(after.items.len(), 10);
    db.execute("UPDATE library_files SET publication_state='published' WHERE library_id=?1 AND publication_state='pending'",[lib.to_string()]).unwrap();
    let op = "query-work-add";
    store.begin_library_integrity(lib, true, op, 2010).unwrap();
    while store
        .advance_library_operation(op, 2011, 100)
        .unwrap()
        .state
        == "applying"
    {}
    let (restored, restored_work) = store.library_item_page_with_stats(&q).unwrap();
    assert_eq!(restored.total, Some(200));
    assert_eq!(restored.items, counted.items);
    assert!(restored_work.vm_steps > 0 && restored_work.statements > 0);
}

#[test]
fn filtered_facets_keep_exclude_own_dimension_counts_and_report_complete_work() {
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
    assert!(
        work.statements > 0 && work.vm_steps > 0,
        "facet work includes lifecycle and result queries"
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
                let mut deterministic_work = work;
                let mut deterministic_after = after;
                deterministic_work.sql_duration_ns = 0;
                deterministic_after.sql_duration_ns = 0;
                assert_eq!(deterministic_work, deterministic_after);
                assert_eq!(store.library_item_page(&q).unwrap(), plain);
                deterministic_work
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

#[test]
fn filtered_facets_work_grows_with_members_not_bucket_times_members_squared() {
    let mut measured = Vec::new();
    for size in [400, 4_000] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let lib = seed(&store, MediaKind::Movie, size);
        let request = FacetRequest {
            library_id: lib,
            user_id: None,
            filter: BrowseFilter {
                genres: vec!["Drama".into()],
                ..Default::default()
            },
            tier: FacetTier::All,
        };
        let (facets, work) = store.library_facets_with_stats(&request).unwrap();
        assert_eq!(facets.total, (size / 2) as u64);
        measured.push(work.vm_steps);
    }
    println!("filtered facet VM 400→4000: {measured:?}");
    assert!(
        measured[1] < measured[0] * 20,
        "filtered facet VM must not grow quadratically: {measured:?}"
    );
}

#[test]
fn actual_protocol_default_sort_reads_only_a_bounded_first_page() {
    let mut measured = Vec::new();
    for size in [400, 4_000] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let lib = seed(&store, MediaKind::Movie, size);
        let mut request = protocol();
        request.parent = ProtocolParent::Library(lib);
        request.limit = 100;
        request.sort = vec![
            ProtocolSort {
                key: ProtocolSortKey::DateCreated,
                descending: true,
            },
            ProtocolSort {
                key: ProtocolSortKey::Name,
                descending: true,
            },
        ];
        let (page, work) = store.protocol_item_page_with_stats(&request).unwrap();
        assert_eq!(page.total, size as u64);
        assert_eq!(page.items.len(), 100);
        measured.push(work.vm_steps);
    }
    println!("actual protocol default VM 400→4000: {measured:?}");
    assert!(
        measured[1] < measured[0] + 2_000,
        "first-page VM should be bounded by the page, not full Library size: {measured:?}"
    );
}

#[test]
fn universal_term_proof_preserves_exclude_own_facets() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = seed(&store, MediaKind::Movie, 40);
    let request = |filter| FacetRequest {
        library_id: lib,
        user_id: None,
        filter,
        tier: FacetTier::All,
    };
    let plain = store
        .library_facets(&request(BrowseFilter::default()))
        .unwrap();
    let all = store
        .library_facets(&request(BrowseFilter {
            genres: vec!["All".into()],
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(all, plain);
    let drama = BrowseFilter {
        genres: vec!["Drama".into()],
        ..Default::default()
    };
    let (half, half_work) = store
        .library_facets_with_stats(&request(drama.clone()))
        .unwrap();
    assert_eq!(half.total, 20);
    assert!(half_work.vm_steps > 0);
    let with_universal = BrowseFilter {
        decades: vec!["2020s".into()],
        stock: vec!["unscraped".into()],
        ..drama
    };
    let three = store.library_facets(&request(with_universal)).unwrap();
    assert_eq!(three, half);
    let missing = store
        .library_facets(&request(BrowseFilter {
            genres: vec!["missing genre".into()],
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(missing.total, 0);
    assert!(
        missing.groups["genres"]
            .iter()
            .any(|bucket| bucket.value == "Drama" && bucket.count == 20)
    );
    for residual in [
        BrowseFilter {
            watch: Some("unwatched".into()),
            ..Default::default()
        },
        BrowseFilter {
            min_rating: Some(7.5),
            ..Default::default()
        },
        BrowseFilter {
            identity: Some("confirmed".into()),
            ..Default::default()
        },
        BrowseFilter {
            require_image: true,
            ..Default::default()
        },
    ] {
        let without_all = store.library_facets(&request(residual.clone())).unwrap();
        let with_all = store
            .library_facets(&request(BrowseFilter {
                genres: vec!["All".into()],
                ..residual
            }))
            .unwrap();
        assert_eq!(with_all, without_all);
    }
}
