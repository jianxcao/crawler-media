use domain::{
    Confidence, EntityScope, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey, UserId,
};
use store::Store;
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};

fn create_media(store: &Store, id: &str, title: &str, kind: MediaKind, year: u16) -> Media {
    let m = Media {
        id: id.parse().unwrap(),
        kind,
        title: title.to_string(),
        year: Some(year),
        original_title: None,
        tmdb_id: Some(id.to_string()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&m).unwrap();
    m
}

fn create_library(
    store: &Store,
    name: &str,
    kind: MediaKind,
    root_path: &str,
) -> (store::Library, String) {
    let lib = store
        .create_library(kind, name, &[root_path], "everyone", true, &[])
        .unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots
        .into_iter()
        .find(|r| r.library_id == Some(lib.id.clone()))
        .unwrap();
    (lib, root.id)
}

fn make_file(
    file_id_str: &str,
    lib_id_str: &str,
    root_id_str: &str,
    media_id_str: &str,
    rel_path: &str,
    kind: MediaKind,
    size: u64,
) -> LibraryFile {
    LibraryFile {
        file_id: file_id_str.parse().unwrap(),
        library_id: lib_id_str.parse().unwrap(),
        root_id: root_id_str.to_string(),
        media_id: media_id_str.parse().unwrap(),
        kind,
        path: rel_path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(size),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash1".to_string()),
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".to_string()),
        codec: Some("hevc".to_string()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some("1080p BluRay".to_string()),
        duration_ms: Some(120_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

#[test]
fn single_table_browse_orders_and_paginates_without_in_memory_sort() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    // Insert 5 movies
    for i in 1..=5 {
        let media_id = format!("00000000-0000-0000-0000-{:012x}", i);
        let title = format!("Movie {:02}", 6 - i); // Movie 05, 04, 03, 02, 01
        create_media(&store, &media_id, &title, MediaKind::Movie, 2020 + i as u16);

        let file_id = format!("00000000-0000-0000-0001-{:012x}", i);
        let path = format!("Movie_{i}.mkv");
        let file = make_file(
            &file_id,
            &lib.id,
            &root_id,
            &media_id,
            &path,
            MediaKind::Movie,
            i * 1_000_000,
        );

        store
            .publish_files(
                &scope,
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }

    // Page 1: Order by Title Asc, limit 2
    let q1 = LibraryQuery {
        library_id: scope.library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 0, limit: 2 },
        include_total: true,
    };

    let p1 = store.library_item_page(&q1).expect("library_item_page");
    assert_eq!(p1.total, Some(5));
    assert_eq!(p1.items.len(), 2);
    assert_eq!(p1.items[0].title, "Movie 01");
    assert_eq!(p1.items[1].title, "Movie 02");

    // Page 2: Offset 2, limit 2
    let q2 = LibraryQuery {
        library_id: scope.library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 2, limit: 2 },
        include_total: false,
    };

    let p2 = store.library_item_page(&q2).expect("library_item_page p2");
    assert_eq!(p2.items.len(), 2);
    assert_eq!(p2.items[0].title, "Movie 03");
    assert_eq!(p2.items[1].title, "Movie 04");

    // Page 3: Offset 4, limit 2
    let q3 = LibraryQuery {
        library_id: scope.library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 4, limit: 2 },
        include_total: false,
    };

    let p3 = store.library_item_page(&q3).expect("library_item_page p3");
    assert_eq!(p3.items.len(), 1);
    assert_eq!(p3.items[0].title, "Movie 05");
}

#[test]
fn library_file_page_paginates_files_deterministically() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id = "00000000-0000-0000-0000-000000000001";
    create_media(&store, media_id, "Show", MediaKind::Tv, 2024);

    let mut publications = Vec::new();
    let mut files = Vec::new();
    for ep in 1..=5 {
        let fid = format!("00000000-0000-0000-0000-{:012x}", ep);
        let path = format!("Show/S01E{:02}.mkv", ep);
        let f = make_file(
            &fid,
            &lib.id,
            &root_id,
            media_id,
            &path,
            MediaKind::Tv,
            1_000_000,
        );
        files.push(f);
    }

    let unit_keys: Vec<UnitKey> = (1..=5).map(|ep| UnitKey::new(1, ep)).collect();
    for (i, f) in files.iter().enumerate() {
        publications.push(StorePublication {
            file: f,
            units: std::slice::from_ref(&unit_keys[i]),
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        });
    }

    store.publish_files(&scope, &publications, 1000).unwrap();

    // Query first page with limit 3
    let p1 = store
        .library_file_page(scope.library_id, media_id.parse().unwrap(), None, 3)
        .unwrap();
    assert_eq!(p1.files.len(), 3);
    assert_eq!(p1.files[0].path.to_str().unwrap(), "/tv/Show/S01E01.mkv");
    assert_eq!(p1.files[2].path.to_str().unwrap(), "/tv/Show/S01E03.mkv");

    // Query next page with limit 3 using cursor
    let cursor = p1.next_cursor.as_deref();
    assert!(cursor.is_some());
    let p2 = store
        .library_file_page(scope.library_id, media_id.parse().unwrap(), cursor, 3)
        .unwrap();
    assert_eq!(p2.files.len(), 2);
    assert_eq!(p2.files[0].path.to_str().unwrap(), "/tv/Show/S01E04.mkv");
    assert_eq!(p2.files[1].path.to_str().unwrap(), "/tv/Show/S01E05.mkv");
}

#[test]
fn library_unit_page_enumerates_units_for_tv_show() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id = "00000000-0000-0000-0000-000000000001";
    create_media(&store, media_id, "Show", MediaKind::Tv, 2024);

    let mut publications = Vec::new();
    let mut files = Vec::new();
    for ep in 1..=3 {
        let fid = format!("00000000-0000-0000-0000-{:012x}", ep);
        let path = format!("Show/S01E{:02}.mkv", ep);
        let f = make_file(
            &fid,
            &lib.id,
            &root_id,
            media_id,
            &path,
            MediaKind::Tv,
            1_000_000,
        );
        files.push(f);
    }

    let unit_keys: Vec<UnitKey> = (1..=3).map(|ep| UnitKey::new(1, ep)).collect();
    for (i, f) in files.iter().enumerate() {
        publications.push(StorePublication {
            file: f,
            units: std::slice::from_ref(&unit_keys[i]),
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        });
    }

    store.publish_files(&scope, &publications, 1000).unwrap();

    let page = store
        .library_unit_page(
            scope.library_id,
            media_id.parse().unwrap(),
            Some(1),
            &BrowsePage::Offset {
                start: 0,
                limit: 10,
            },
        )
        .expect("library_unit_page");

    assert_eq!(page.total, 3);
    assert_eq!(page.units.len(), 3);
    assert_eq!(page.units[0].key.episode, 1);
    assert_eq!(page.units[2].key.episode, 3);
}

#[test]
fn library_item_detail_hydrates_item_metadata_and_bounded_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id = "00000000-0000-0000-0000-000000000001";
    create_media(&store, media_id, "Inception", MediaKind::Movie, 2010);

    let fid = "00000000-0000-0000-0000-000000000011";
    let f = make_file(
        fid,
        &lib.id,
        &root_id,
        media_id,
        "Inception.mkv",
        MediaKind::Movie,
        5_000_000,
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &f,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();

    // Adopt metadata
    let raw = r#"{"title":"Inception","overview":"Dream within a dream"}"#;
    let pref = domain::MetadataPreferences {
        language_priority: vec!["en-US".into()],
        cert_country_priority: vec!["US".into()],
    };
    let norm = media::normalize_metadata("tmdb", EntityScope::Work, raw, &pref).unwrap();
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media_id.parse().unwrap(),
                scope: EntityScope::Work,
                locale: "en-US".into(),
                source: "tmdb".into(),
                provider_entity_id: "27205".into(),
                provider_work_id: "27205".into(),
                fetched_at: 1000,
                normalized: norm,
                expected_revision: None,
                preferences: pref,
            },
            1000,
        )
        .unwrap();

    let detail = store
        .library_item_detail(scope.library_id, media_id.parse().unwrap(), "en-US")
        .expect("library_item_detail")
        .expect("must exist");

    assert_eq!(detail.item.title, "Inception");
    assert_eq!(detail.files.len(), 1);
    assert_eq!(
        detail.metadata.overview.value().unwrap(),
        "Dream within a dream"
    );
}

#[test]
fn protocol_item_page_pagination_and_wire_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (lib, root_id) =
        create_library(&store, "Protocol Movies", MediaKind::Movie, "/media/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id1 = "00000000-0000-0000-0000-000000000001";
    let media_id2 = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id1, "Alien", MediaKind::Movie, 1979);
    create_media(&store, media_id2, "Blade Runner", MediaKind::Movie, 1982);

    let file1 = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id1,
        "Alien.mkv",
        MediaKind::Movie,
        100,
    );
    let file2 = make_file(
        "00000000-0000-0000-0000-000000000012",
        &lib.id,
        &root_id,
        media_id2,
        "Blade Runner.mkv",
        MediaKind::Movie,
        200,
    );

    store
        .publish_files(
            &scope,
            &[
                StorePublication {
                    file: &file1,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                },
                StorePublication {
                    file: &file2,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                },
            ],
            1000,
        )
        .unwrap();

    let user_id = UserId::new();
    let query = domain::ProtocolItemQuery {
        user_id,
        parent: domain::ProtocolParent::Library(scope.library_id),
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: None,
        genres: vec![],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: None,
        include_types: vec![domain::ProtocolItemType::Movie],
        recursive: true,
        is_folder: None,
        sort: vec![domain::ProtocolSort {
            key: domain::ProtocolSortKey::Name,
            descending: false,
        }],
        offset: 0,
        limit: 1,
    };

    let page = store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    let first = &page.items[0];
    assert_eq!(
        first.item,
        domain::TypedItemId::Work(media_id1.parse().unwrap())
    );
    let wire_item = first.item.to_wire_string();
    assert!(wire_item.starts_with("w1_"));
    assert_eq!(
        domain::TypedItemId::from_wire_string(&wire_item),
        Some(first.item.clone())
    );

    let pref_src = first.preferred_source.as_ref().unwrap();
    let wire_src = pref_src.to_wire_string();
    assert!(wire_src.starts_with("f1_"));
    assert_eq!(
        domain::PhysicalSourceId::from_wire_string(&wire_src),
        Some(pref_src.clone())
    );

    // Page 2
    let mut query2 = query.clone();
    query2.offset = 1;
    let page2 = store.protocol_item_page(&query2).unwrap();
    assert_eq!(page2.total, 2);
    assert_eq!(page2.items.len(), 1);
    assert_eq!(
        page2.items[0].item,
        domain::TypedItemId::Work(media_id2.parse().unwrap())
    );

    // Unknown parent returns empty
    let mut query_unknown = query.clone();
    query_unknown.parent = domain::ProtocolParent::Unknown;
    let page_unknown = store.protocol_item_page(&query_unknown).unwrap();
    assert_eq!(page_unknown.total, 0);
    assert!(page_unknown.items.is_empty());
}

#[test]
fn library_file_page_rejects_unknown_presence_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "Test Lib", MediaKind::Movie, "/test");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id_str = "00000000-0000-0000-0000-000000000001";
    let media = create_media(&store, media_id_str, "Test Movie", MediaKind::Movie, 2024);

    let file_id_str1 = "00000000-0000-0000-0001-000000000001";
    let file1 = make_file(
        file_id_str1,
        &lib.id,
        &root_id,
        media_id_str,
        "test1.mkv",
        MediaKind::Movie,
        1_000_000,
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file1,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();

    // 1. 验证正常查询为 Ok
    let page = store
        .library_file_page(scope.library_id, media.id, None, 10)
        .unwrap();
    assert_eq!(page.files.len(), 1);

    // 2. 注入非法 presence_state ('bogus')：断言显式拒绝
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    db.execute(
        "UPDATE library_files SET presence_state = 'bogus' WHERE file_id = ?1",
        rusqlite::params![file1.file_id.to_string()],
    )
    .unwrap();

    let err_presence = store
        .library_file_page(scope.library_id, media.id, None, 10)
        .unwrap_err();
    let msg_presence = err_presence.to_string();
    assert!(
        msg_presence.contains("bogus") && msg_presence.contains(&file1.file_id.to_string()),
        "presence rejection must explicitly report bogus state and file_id: {msg_presence}"
    );

    // 3. 证明非法 publication_state 在公共 seam 不可达：
    // 原因：crates/store/src/library_query/files.rs:58 与 :73 的两处查询分支均硬编码了
    // `AND publication_state = 'published'` 过滤条件。
    // 当数据库中存在 publication_state='bogus'（或非 published）的行时，该行在 SQL 结果集阶段
    // 直接被过滤排除，根本不会返回给映射闭包，因此返回 Ok 且列表为空。
    // 该分支（files.rs:117-122 中的 ok_or_else(publication)）属于防御性实现，公共 seam 无法触发其错误分支。
    db.execute(
        "UPDATE library_files SET presence_state = 'present', publication_state = 'bogus' WHERE file_id = ?1",
        rusqlite::params![file1.file_id.to_string()],
    )
    .unwrap();

    let page_bogus_pub = store
        .library_file_page(scope.library_id, media.id, None, 10)
        .unwrap();
    assert!(
        page_bogus_pub.files.is_empty(),
        "files with publication_state != 'published' must be filtered out at SQL boundary before mapping"
    );
    let stored_bogus_pub: String = db
        .query_row(
            "SELECT publication_state FROM library_files WHERE file_id = ?1",
            rusqlite::params![file1.file_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_bogus_pub, "bogus",
        "the row must still exist with the bogus value: it is the SQL filter, not deletion, that hides it"
    );
    assert!(
        page_bogus_pub.next_cursor.is_none(),
        "an empty page cannot carry a cursor"
    );
}
