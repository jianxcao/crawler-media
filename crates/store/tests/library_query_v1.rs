use domain::{
    Confidence, EntityScope, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey, UserId,
};
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::Store;

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

fn create_library(store: &Store, name: &str, kind: MediaKind, root_path: &str) -> (store::Library, String) {
    let lib = store
        .create_library(kind, name, &[root_path], "everyone", true, &[])
        .unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots.into_iter().find(|r| r.library_id == Some(lib.id.clone())).unwrap();
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
        let file = make_file(&file_id, &lib.id, &root_id, &media_id, &path, MediaKind::Movie, i * 1_000_000);

        store.publish_files(&scope, &[StorePublication {
            file: &file,
            units: &[UnitKey::WHOLE],
            expected_version: None,
            probe_required: false,
        }], 1000).unwrap();
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
        let f = make_file(&fid, &lib.id, &root_id, media_id, &path, MediaKind::Tv, 1_000_000);
        files.push(f);
    }

    let unit_keys: Vec<UnitKey> = (1..=5).map(|ep| UnitKey::new(1, ep)).collect();
    for (i, f) in files.iter().enumerate() {
        publications.push(StorePublication {
            file: f,
            units: std::slice::from_ref(&unit_keys[i]),
            expected_version: None,
            probe_required: false,
        });
    }

    store.publish_files(&scope, &publications, 1000).unwrap();

    // Query first page with limit 3
    let p1 = store.library_file_page(scope.library_id, media_id.parse().unwrap(), None, 3).unwrap();
    assert_eq!(p1.files.len(), 3);
    assert_eq!(p1.files[0].path.to_str().unwrap(), "Show/S01E01.mkv");
    assert_eq!(p1.files[2].path.to_str().unwrap(), "Show/S01E03.mkv");

    // Query next page with limit 3 using cursor
    let cursor = p1.next_cursor.as_deref();
    assert!(cursor.is_some());
    let p2 = store.library_file_page(scope.library_id, media_id.parse().unwrap(), cursor, 3).unwrap();
    assert_eq!(p2.files.len(), 2);
    assert_eq!(p2.files[0].path.to_str().unwrap(), "Show/S01E04.mkv");
    assert_eq!(p2.files[1].path.to_str().unwrap(), "Show/S01E05.mkv");
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
        let f = make_file(&fid, &lib.id, &root_id, media_id, &path, MediaKind::Tv, 1_000_000);
        files.push(f);
    }

    let unit_keys: Vec<UnitKey> = (1..=3).map(|ep| UnitKey::new(1, ep)).collect();
    for (i, f) in files.iter().enumerate() {
        publications.push(StorePublication {
            file: f,
            units: std::slice::from_ref(&unit_keys[i]),
            expected_version: None,
            probe_required: false,
        });
    }

    store.publish_files(&scope, &publications, 1000).unwrap();

    let page = store
        .library_unit_page(
            scope.library_id,
            media_id.parse().unwrap(),
            Some(1),
            &BrowsePage::Offset { start: 0, limit: 10 },
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
    let f = make_file(fid, &lib.id, &root_id, media_id, "Inception.mkv", MediaKind::Movie, 5_000_000);
    store.publish_files(&scope, &[StorePublication {
        file: &f,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        probe_required: false,
    }], 1000).unwrap();

    // Adopt metadata
    let raw = r#"{"title":"Inception","overview":"Dream within a dream"}"#;
    let pref = domain::MetadataPreferences {
        language_priority: vec!["en-US".into()],
        cert_country_priority: vec!["US".into()],
    };
    let norm = media::normalize_metadata("tmdb", EntityScope::Work, raw, &pref).unwrap();
    store.adopt_metadata(&domain::MetadataAdoption {
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
    }, 1000).unwrap();

    let detail = store
        .library_item_detail(scope.library_id, media_id.parse().unwrap(), "en-US")
        .expect("library_item_detail")
        .expect("must exist");

    assert_eq!(detail.item.title, "Inception");
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.metadata.overview.value().unwrap(), "Dream within a dream");
}
