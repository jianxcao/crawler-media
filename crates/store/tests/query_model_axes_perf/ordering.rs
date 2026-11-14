use super::*;
use domain::TypedItemId;

fn sorted_ids(store: &Store, query: &domain::ProtocolItemQuery) -> Vec<domain::MediaId> {
    store
        .protocol_item_page(query)
        .unwrap()
        .items
        .into_iter()
        .map(|row| match row.item {
            TypedItemId::Work(id) => id,
            _ => panic!("Work expected"),
        })
        .collect()
}

pub(super) fn run() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = fixed_library(&store, MediaKind::Movie, "axis-unicode-sort");
    support::seed_into(
        &store,
        MediaKind::Movie,
        0,
        100,
        library,
        "/query-work/movie",
    );
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    for (index, title) in ["A", "a", "Ä", "ä"].into_iter().enumerate() {
        let id = media_id(MediaKind::Movie, index).to_string();
        db.execute(
            "UPDATE media SET title=?2 WHERE media_id=?1",
            rusqlite::params![id, title],
        )
        .unwrap();
        db.execute(
            "UPDATE library_files SET created_ns=100 WHERE media_id=?1",
            [id],
        )
        .unwrap();
    }
    drop(db);
    repair(&store, library, "unicode-ties");
    let ids = (0..4)
        .map(|index| media_id(MediaKind::Movie, index))
        .collect::<Vec<_>>();
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 100;
    query.sort = vec![
        ProtocolSort {
            key: ProtocolSortKey::DateCreated,
            descending: true,
        },
        ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: true,
        },
    ];
    assert_eq!(
        &sorted_ids(&store, &query)[..4],
        &[ids[2], ids[3], ids[0], ids[1]]
    );
    measure("unicode_multisort_desc_ties", 100, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        let first: Vec<_> = page
            .items
            .iter()
            .take(4)
            .map(|row| row.item.media_id())
            .collect();
        assert_eq!(first, [ids[2], ids[3], ids[0], ids[1]]);
        (page.total, work)
    });
    query.sort[1].descending = false;
    assert_eq!(
        &sorted_ids(&store, &query)[..4],
        &[ids[0], ids[1], ids[2], ids[3]]
    );
    measure("unicode_multisort_asc_ties", 100, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        (page.total, work)
    });
    query.sort.remove(0);
    measure("unicode_name_asc", 100, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        assert_eq!(page.items[0].item.media_id(), ids[0]);
        assert_eq!(page.items[1].item.media_id(), ids[1]);
        (page.total, work)
    });
    query.sort[0].descending = true;
    measure("unicode_name_desc", 100, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        assert_eq!(page.items[0].item.media_id(), ids[2]);
        assert_eq!(page.items[1].item.media_id(), ids[3]);
        (page.total, work)
    });
}
