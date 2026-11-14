use super::*;
use store::playback::PlaybackMutation;

fn add_overlap_library(store: &Store, size: usize, ordinal: usize) {
    let root = format!("/query-work/overlap/{ordinal}");
    let library = store
        .create_library(MediaKind::Movie, "Overlap", &[&root], "everyone", true, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|row| row.library_id == library.id)
        .unwrap()
        .id;
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..size {
        let file = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("overlap-{ordinal}-{index}").as_bytes(),
        )
        .to_string();
        let media = media_id(MediaKind::Movie, index).to_string();
        let path = format!("{root}/{index}.mkv");
        tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence)
            VALUES(?1,?2,?3,?4,'movie',?5,?5,'present','published',1,'v1','probe','high')",
            rusqlite::params![file, library.id, root_id, media, path]).unwrap();
        tx.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES(?1,?2,?3,-1,-1)",
            rusqlite::params![file, library.id, media]).unwrap();
    }
    tx.commit().unwrap();
    repair(
        store,
        library.id.parse().unwrap(),
        &format!("overlap-{ordinal}"),
    );
}

fn measure_overlap(store: &Store, size: usize, libraries: usize) {
    let mut query = support::protocol();
    query.parent = ProtocolParent::Root;
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
    measure(&format!("overlap_{libraries}_root"), size as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        assert_eq!(page.items.len(), size.min(100));
        let ids = page
            .items
            .iter()
            .map(|row| &row.item)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), page.items.len());
        (page.total, work)
    });
    db_sizes(store, &format!("overlap_{libraries}"));
}

pub(super) fn overlap(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = fixed_library(&store, MediaKind::Movie, "axis-overlap-first");
    support::seed_into(
        &store,
        MediaKind::Movie,
        0,
        size,
        first,
        "/query-work/movie",
    );
    add_overlap_library(&store, size, 2);
    measure_overlap(&store, size, 2);
    for ordinal in 3..=5 {
        add_overlap_library(&store, size, ordinal);
    }
    measure_overlap(&store, size, 5);
}

fn measure_user(store: &Store, library: LibraryId, _size: usize, marked: usize, label: &str) {
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 100;
    query.is_favorite = Some(true);
    query.is_played = Some(true);
    measure(label, marked as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        assert_eq!(page.items.len(), marked.min(100));
        (page.total, work)
    });
    let facets = FacetRequest {
        library_id: library,
        user_id: Some(query.user_id),
        filter: BrowseFilter {
            watch: Some("played".into()),
            genres: vec!["Drama".into()],
            ..Default::default()
        },
        tier: FacetTier::All,
    };
    let expected = marked.div_ceil(2) as u64;
    measure(&format!("{label}_watch_genre"), expected, || {
        let (page, work) = store.library_facets_with_stats(&facets).unwrap();
        (page.total, work)
    });
}

pub(super) fn users(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = fixed_library(&store, MediaKind::Movie, "axis-users");
    support::seed_into(
        &store,
        MediaKind::Movie,
        0,
        size,
        library,
        "/query-work/movie",
    );
    measure_user(&store, library, size, 0, "user_empty");
    let user = support::protocol().user_id;
    for (end, label) in [
        (size / 100, "user_sparse_1pct"),
        (size, "user_dense_100pct"),
    ] {
        for index in if end == size { size / 100 } else { 0 }..end {
            store
                .mutate_playback(
                    media_id(MediaKind::Movie, index),
                    &PlaybackMutation::marks(user, -1, -1, Some(true), Some(true)),
                    8_000,
                )
                .unwrap();
        }
        measure_user(&store, library, size, end, label);
        db_sizes(&store, label);
    }
}
