//! Public protocol paging work with raw facts repaired into the current read model.
use domain::{MediaKind, ProtocolParent, ProtocolSort, ProtocolSortKey, TypedItemId};
use store::Store;

#[path = "query_work/support.rs"]
mod support;

fn expected_first_ids(store: &Store, library: domain::LibraryId) -> Vec<TypedItemId> {
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let mut stmt = db.prepare("SELECT DISTINCT m.media_id,m.title FROM media m JOIN library_files f ON f.media_id=m.media_id WHERE f.library_id=?1 ORDER BY m.title,m.media_id LIMIT 100").unwrap();
    stmt.query_map([library.to_string()], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|id| TypedItemId::Work(id.unwrap().parse().unwrap()))
        .collect()
}

#[test]
fn single_library_first_page_does_not_sort_or_window_the_entire_hitset() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, MediaKind::Movie, 400);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 100;
    let (small, small_work) = store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(small.total, 400);
    assert_eq!(small.items.len(), 100);
    let expected = expected_first_ids(&store, library);
    assert_eq!(
        small
            .items
            .iter()
            .map(|r| r.item.clone())
            .collect::<Vec<_>>(),
        expected
    );
    support::seed_into(
        &store,
        MediaKind::Movie,
        400,
        3600,
        library,
        "/query-work/movie",
    );
    let (large, large_work) = store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(large.total, 4000);
    assert_eq!(
        large
            .items
            .iter()
            .map(|r| r.item.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        large_work.vm_steps < small_work.vm_steps + 2000,
        "first page work grew: {small_work:?} -> {large_work:?}"
    );
    assert!(large_work.sort_operations <= small_work.sort_operations + 1);
}

#[test]
fn all_hit_genre_keeps_exact_total_and_bounded_first_page_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let root = "/query-work/all-drama";
    let library: domain::LibraryId = store
        .create_library(
            MediaKind::Movie,
            "All Drama",
            &[root],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
        .parse()
        .unwrap();
    support::seed_into_with_genre(&store, MediaKind::Movie, 0, 400, library, root, true);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.genres = vec!["Drama".into()];
    query.limit = 100;
    let (small, small_work) = store.protocol_item_page_with_stats(&query).unwrap();
    let expected = expected_first_ids(&store, library);
    assert_eq!(
        small
            .items
            .iter()
            .map(|r| r.item.clone())
            .collect::<Vec<_>>(),
        expected
    );
    support::seed_into_with_genre(&store, MediaKind::Movie, 400, 3600, library, root, true);
    let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
    assert_eq!(page.total, 4000);
    assert_eq!(
        page.items
            .iter()
            .map(|r| r.item.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        work.vm_steps < small_work.vm_steps + 2000,
        "genre first page work grew: {small_work:?} -> {work:?}"
    );
    assert!(work.sort_operations <= small_work.sort_operations + 1);
}

#[test]
fn empty_and_past_end_pages_keep_the_exact_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, MediaKind::Movie, 4);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 4;
    let full = store.protocol_item_page(&query).unwrap();
    assert_eq!(full.total, 4);
    assert_eq!(full.items.len(), 4);
    query.limit = 0;
    let zero = store.protocol_item_page(&query).unwrap();
    assert_eq!(zero.total, 4);
    assert!(zero.items.is_empty());
    query.limit = 4;
    query.offset = 8;
    let past = store.protocol_item_page(&query).unwrap();
    assert_eq!(past.total, 4);
    assert!(past.items.is_empty());
}

#[test]
fn unrelated_blocked_library_does_not_hide_indexed_page() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, MediaKind::Movie, 4);
    let other = store
        .create_library(
            MediaKind::Movie,
            "Blocked",
            &["/query-work/blocked"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.begin_library_removal(&other.id).unwrap();
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 4;
    let page = store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 4);
    assert_eq!(page.items.len(), 4);
    assert!(page.items.iter().all(|item| item.library_id == library));
}

#[test]
fn source_uses_actual_revision_then_smallest_file_id_after_page_selection() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, MediaKind::Movie, 1);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    let first = store.protocol_item_page(&query).unwrap();
    let domain::TypedItemId::Work(media) = first.items[0].item else {
        panic!("work expected")
    };
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == library.to_string())
        .unwrap()
        .id;
    let db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    let mut ids = [
        domain::LedgerId::new().to_string(),
        domain::LedgerId::new().to_string(),
    ];
    ids.sort();
    for (n, id) in ids.iter().enumerate() {
        db.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES(?1,?2,?3,?4,'movie',?5,?5,'present','published',10,'v2','probe','high')",
            rusqlite::params![id, library.to_string(), root, media.to_string(), format!("/query-work/movie/new-{n}.mkv")]).unwrap();
    }
    db.execute(
        "UPDATE library_files SET file_revision=20,presence_state='missing' WHERE file_id=?1",
        [&ids[1]],
    )
    .unwrap();
    let selected = store.protocol_item_page(&query).unwrap();
    assert_eq!(selected.total, 1);
    let source = selected.items[0].preferred_source.as_ref().unwrap();
    assert_eq!(source.file_id.to_string(), ids[0]);
    assert_eq!(source.revision, 10);
    db.execute(
        "UPDATE library_files SET presence_state='present',file_revision=10 WHERE file_id=?1",
        [&ids[1]],
    )
    .unwrap();
    let tied = store.protocol_item_page(&query).unwrap();
    assert_eq!(
        tied.items[0]
            .preferred_source
            .as_ref()
            .unwrap()
            .file_id
            .to_string(),
        ids[0]
    );
}

#[test]
fn complete_name_and_date_orders_keep_original_title_after_explicit_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, MediaKind::Movie, 4);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 4;
    let ids = store
        .protocol_item_page(&query)
        .unwrap()
        .items
        .into_iter()
        .map(|r| {
            let TypedItemId::Work(id) = r.item else {
                panic!("work expected")
            };
            id
        })
        .collect::<Vec<_>>();
    let db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    for (id, title) in ids.iter().zip(["A", "a", "Ä", "ä"]) {
        db.execute(
            "UPDATE media SET title=?2 WHERE media_id=?1",
            rusqlite::params![id.to_string(), title],
        )
        .unwrap();
    }
    let operation = format!("sort-{}", uuid::Uuid::new_v4());
    store
        .begin_library_integrity(library, true, &operation, 2000)
        .unwrap();
    while store
        .advance_library_operation(&operation, 2001, 100)
        .unwrap()
        .state
        == "applying"
    {}
    for (id, added) in ids.iter().zip([100_i64, 200, 400, 300]) {
        db.execute(
            "UPDATE library_items SET added_at_sort=?2 WHERE media_id=?1",
            rusqlite::params![id.to_string(), added],
        )
        .unwrap();
    }
    let order = |query: &domain::ProtocolItemQuery| {
        store
            .protocol_item_page(query)
            .unwrap()
            .items
            .into_iter()
            .map(|r| r.item)
            .collect::<Vec<_>>()
    };
    query.sort.clear();
    assert_eq!(
        order(&query),
        [ids[2], ids[3], ids[1], ids[0]].map(TypedItemId::Work)
    );
    query.sort = vec![ProtocolSort {
        key: ProtocolSortKey::Name,
        descending: false,
    }];
    assert_eq!(
        order(&query),
        ids.iter()
            .copied()
            .map(TypedItemId::Work)
            .collect::<Vec<_>>()
    );
    query.sort[0].descending = true;
    assert_eq!(
        order(&query),
        [ids[2], ids[3], ids[0], ids[1]].map(TypedItemId::Work)
    );
    query.sort = vec![
        ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: false,
        },
        ProtocolSort {
            key: ProtocolSortKey::DateCreated,
            descending: true,
        },
    ];
    assert_eq!(
        order(&query),
        [ids[1], ids[0], ids[2], ids[3]].map(TypedItemId::Work)
    );
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
        order(&query),
        [ids[2], ids[3], ids[1], ids[0]].map(TypedItemId::Work)
    );
    db.execute(
        "UPDATE library_items SET added_at_sort=100 WHERE media_id=?1",
        [ids[1].to_string()],
    )
    .unwrap();
    assert_eq!(
        order(&query),
        [ids[2], ids[3], ids[0], ids[1]].map(TypedItemId::Work),
        "original title breaks identical DateCreated and Unicode-folded Name ties"
    );
}

fn publish_show(
    store: &Store,
    root: &std::path::Path,
    title: &str,
    units: &[(u32, u32)],
) -> domain::MediaId {
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&media).unwrap();
    for &(season, episode) in units {
        let path = root.join(format!("{title}-S{season:02}E{episode:02}.mkv"));
        std::fs::write(&path, b"payload").unwrap();
        store::test_support::publish_fixture_file(
            store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                season: Some(season),
                episode: Some(episode),
                ..Default::default()
            },
        )
        .unwrap();
    }
    media.id
}

fn assert_full_and_slices(
    store: &Store,
    query: &mut domain::ProtocolItemQuery,
    expected: &[TypedItemId],
) {
    query.limit = expected.len();
    query.offset = 0;
    let full = store.protocol_item_page(query).unwrap();
    assert_eq!(full.total, expected.len() as u64);
    assert_eq!(
        full.items.into_iter().map(|r| r.item).collect::<Vec<_>>(),
        expected
    );
    query.limit = 2;
    let mut slices = Vec::new();
    for offset in (0..expected.len()).step_by(2) {
        query.offset = offset;
        let page = store.protocol_item_page(query).unwrap();
        assert_eq!(page.total, expected.len() as u64);
        slices.extend(page.items.into_iter().map(|r| r.item));
    }
    assert_eq!(slices, expected);
}

#[test]
fn unit_and_mixed_orders_are_complete_across_ties_and_page_slices() {
    use domain::{ProtocolItemType, UnitKey};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let root = dir.path().join("shows");
    std::fs::create_dir_all(&root).unwrap();
    let library: domain::LibraryId = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
        .parse()
        .unwrap();
    let alpha = publish_show(&store, &root, "Alpha", &[(1, 1), (2, 1)]);
    let beta = publish_show(&store, &root, "Beta", &[(1, 1), (1, 2)]);
    let unit = |media, season, episode| TypedItemId::Unit {
        media,
        key: UnitKey::new(season, episode),
    };
    let (first, second) = if alpha.to_string() < beta.to_string() {
        (alpha, beta)
    } else {
        (beta, alpha)
    };
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.include_types = vec![ProtocolItemType::Episode];
    query.sort.clear();
    let default_units = [
        unit(first, 1, 1),
        unit(second, 1, 1),
        unit(beta, 1, 2),
        unit(alpha, 2, 1),
    ];
    assert_full_and_slices(&store, &mut query, &default_units);
    query.sort = vec![ProtocolSort {
        key: ProtocolSortKey::Name,
        descending: false,
    }];
    let named_units = [
        unit(alpha, 1, 1),
        unit(alpha, 2, 1),
        unit(beta, 1, 1),
        unit(beta, 1, 2),
    ];
    assert_full_and_slices(&store, &mut query, &named_units);
    query.include_types = vec![ProtocolItemType::Series, ProtocolItemType::Episode];
    query.sort.clear();
    let default_mixed = [
        TypedItemId::Work(first),
        TypedItemId::Work(second),
        unit(first, 1, 1),
        unit(second, 1, 1),
        unit(beta, 1, 2),
        unit(alpha, 2, 1),
    ];
    assert_full_and_slices(&store, &mut query, &default_mixed);
    query.sort = vec![ProtocolSort {
        key: ProtocolSortKey::Name,
        descending: false,
    }];
    let named_mixed = [
        TypedItemId::Work(alpha),
        unit(alpha, 1, 1),
        unit(alpha, 2, 1),
        TypedItemId::Work(beta),
        unit(beta, 1, 1),
        unit(beta, 1, 2),
    ];
    assert_full_and_slices(&store, &mut query, &named_mixed);
}
