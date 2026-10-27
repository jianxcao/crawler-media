use domain::MediaKind;
use store::{
    Store,
    library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery},
};
#[path = "query_work/support.rs"]
#[allow(dead_code)]
mod support;

fn measure(count: usize) -> Vec<store::library_query::items::QueryWorkStats> {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library_id = support::seed(&store, MediaKind::Movie, count);
    [vec![], vec!["Drama".into()]]
        .into_iter()
        .enumerate()
        .map(|(index, genres)| {
            let query = LibraryQuery {
                library_id,
                user_id: None,
                filter: BrowseFilter {
                    genres,
                    ..Default::default()
                },
                order: BrowseOrder::Title,
                descending: false,
                page: BrowsePage::Offset {
                    start: 0,
                    limit: 10,
                },
                include_total: true,
            };
            let (page, work) = store.library_item_page_with_stats(&query).unwrap();
            assert_eq!(page.total, Some((count / (index + 1)) as u64));
            assert_eq!(page.items.len(), 10);
            work
        })
        .collect()
}

#[test]
fn unfiltered_and_single_genre_counts_use_marginals() {
    let small = measure(400);
    let large = measure(4000);
    for (small, large) in small.iter().zip(&large) {
        println!("small={small:?} large={large:?}");
        assert!(large.vm_steps < small.vm_steps + 2000);
    }
}

#[test]
fn overlapping_unions_intersections_and_arbitrary_rating_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library_id = support::seed(&store, MediaKind::Movie, 4);
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let ids: Vec<String> = db
        .prepare("SELECT media_id FROM media ORDER BY title")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (index, id) in ids.iter().enumerate() {
        let names = if index == 0 {
            vec!["Drama", "Comedy"]
        } else if index % 2 == 0 {
            vec!["Drama"]
        } else {
            vec!["Comedy"]
        };
        let genres: Vec<_> = names
            .into_iter()
            .map(|name| serde_json::json!({"source":"fixture","name":name,"filter_key":name}))
            .collect();
        let fields = serde_json::from_value(serde_json::json!({
            "genres":{"state":"value","value":genres},
            "countries":{"state":"value","value":[if index < 2 {"US"} else {"JP"}]},
            "rating":{"state":"value","value":if index % 2 == 0 {7.4} else {7.5}}
        }))
        .unwrap();
        store
            .set_metadata_overrides(
                id.parse().unwrap(),
                domain::EntityScope::Work,
                "zh-CN",
                &fields,
                0,
            )
            .unwrap();
    }
    for (filter, expected) in [
        (
            BrowseFilter {
                genres: vec!["Drama".into(), "Comedy".into(), "Drama".into()],
                ..Default::default()
            },
            vec![0, 1, 2, 3],
        ),
        (
            BrowseFilter {
                genres: vec!["Drama".into()],
                countries: vec!["us".into()],
                ..Default::default()
            },
            vec![0],
        ),
        (
            BrowseFilter {
                min_rating: Some(7.5),
                ..Default::default()
            },
            vec![1, 3],
        ),
    ] {
        let query = LibraryQuery {
            library_id,
            user_id: None,
            filter,
            order: BrowseOrder::Title,
            descending: false,
            page: BrowsePage::Offset {
                start: 0,
                limit: 10,
            },
            include_total: true,
        };
        let page = store.library_item_page(&query).unwrap();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|n| ids[n].parse::<domain::MediaId>().unwrap())
            .collect();
        assert_eq!(page.total, Some(expected.len() as u64));
        assert_eq!(
            page.items.iter().map(|i| i.media_id).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            store.library_item_count(&query).unwrap(),
            expected.len() as u64
        );
    }
}
