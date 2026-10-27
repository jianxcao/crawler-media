use super::*;
use domain::ProtocolItemType;

fn measure_page(store: &Store, label: &str, query: &domain::ProtocolItemQuery, expected: usize) {
    measure(label, expected as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(query).unwrap();
        assert_eq!(page.items.len(), expected.min(query.limit));
        (page.total, work)
    });
}

pub(super) fn run(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = fixed_library(&store, MediaKind::Tv, "axis-tv-hierarchy");
    support::seed_into(&store, MediaKind::Tv, 0, size, library, "/query-work/tv");
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute(
        "INSERT INTO library_file_units(file_id,library_id,media_id,season,episode)
         SELECT file_id,library_id,media_id,1,1 FROM library_files WHERE library_id=?1",
        [library.to_string()],
    )
    .unwrap();
    drop(db);
    repair(&store, library, "tv-units");
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|row| row.library_id == library.to_string())
        .unwrap()
        .id;
    let first = media_id(MediaKind::Tv, 0);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 100;
    query.include_types = vec![ProtocolItemType::Series];
    measure_page(&store, "hierarchy_series", &query, size);
    query.parent = ProtocolParent::Series(first);
    query.include_types = vec![ProtocolItemType::Season];
    measure_page(&store, "hierarchy_season", &query, 1);
    query.parent = ProtocolParent::Season(first, 1);
    query.series_id = Some(first);
    query.season = Some((first, 1));
    query.include_types = vec![ProtocolItemType::Episode];
    query.sort = vec![
        ProtocolSort {
            key: ProtocolSortKey::SeasonNumber,
            descending: false,
        },
        ProtocolSort {
            key: ProtocolSortKey::EpisodeNumber,
            descending: false,
        },
    ];
    measure_page(&store, "hierarchy_episode_explicit_unit_default", &query, 1);
    query.parent = ProtocolParent::Folder { root_id };
    query.series_id = None;
    query.season = None;
    measure_page(&store, "hierarchy_folder_episodes", &query, size);
    query.parent = ProtocolParent::Library(library);
    query.include_types = vec![ProtocolItemType::Series, ProtocolItemType::Episode];
    query.sort.clear();
    measure_page(&store, "hierarchy_mixed", &query, size * 2);
    db_sizes(&store, "hierarchy_tv");
}
