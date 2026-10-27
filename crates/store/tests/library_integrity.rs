use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaKind, QualitySource, UnitKey, UserId,
};
use rusqlite::params;
use store::library_query::LibraryQuery;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::{Store, StoreError};

fn create_media(store: &Store, id: &str, title: &str) -> Media {
    let m = Media {
        id: id.parse().unwrap(),
        kind: MediaKind::Movie,
        title: title.to_string(),
        year: Some(2024),
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
        .find(|r| r.library_id == lib.id.clone())
        .unwrap();
    (lib, root.id)
}

fn make_file(library_id: LibraryId, root_id: String, media: &Media, rel_path: &str) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id,
        root_id,
        media_id: media.id,
        kind: MediaKind::Movie,
        path: rel_path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1024),
        stat_known: true,
        created_ns: Some(0),
        modified_ns: Some(0),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

#[test]
fn integrity_check_and_repair_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (lib, root_id) = create_library(&store, "Movies", MediaKind::Movie, "/movies");
    let library_id: LibraryId = lib.id.parse().unwrap();
    let user_id = UserId::new();

    let m1_id = "00000000-0000-0000-0000-000000000001";
    let media = create_media(&store, m1_id, "Movie 1");

    let scope = WriteScope {
        library_id,
        config_revision: 0,
    };
    let file = make_file(library_id, root_id, &media, "/movies/m1.mkv");
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

    store
        .mutate_playback(
            media.id,
            &store::playback::PlaybackMutation::marks(
                user_id,
                store::UNIT_WHOLE,
                store::UNIT_WHOLE,
                Some(true),
                Some(true),
            ),
            1000,
        )
        .unwrap();

    let query = LibraryQuery {
        library_id,
        user_id: None,
        filter: Default::default(),
        order: store::library_query::BrowseOrder::Title,
        descending: false,
        page: store::library_query::BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: true,
    };

    // Snapshot expected facts before tampering
    let expected_item_page = store.library_item_page(&query).unwrap();
    let expected_from_original_facts = expected_item_page.items[0].clone();
    let expected_user_summary = store
        .get_user_library_summary(user_id, library_id, media.id)
        .unwrap()
        .unwrap();

    // Tamper by deleting rows directly from SQLite
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute(
        "DELETE FROM user_library_items WHERE media_id = ?1",
        params![media.id.to_string()],
    )
    .unwrap();
    db.execute(
        "DELETE FROM library_items WHERE media_id = ?1",
        params![media.id.to_string()],
    )
    .unwrap();

    // 1. Begin integrity check
    let op_id = "00000000-0000-0000-0001-000000000001";
    let status = store
        .begin_library_integrity(library_id, false, op_id, 1000)
        .unwrap();
    assert_eq!(status.state, "planned");

    let res = store.advance_library_operation(op_id, 1000, 100).unwrap();
    assert_eq!(res.state, "succeeded");

    let report = store.library_integrity_report(op_id).unwrap();
    assert!(report.missing_items.contains(&media.id));
    assert!(report.missing_user_items.contains(&(user_id, media.id)));

    // Failed check blocks reading items
    let read_err = store.library_item_page(&query).unwrap_err();
    assert!(matches!(&read_err, StoreError::Protected(msg) if msg.contains("library.integrity")));

    // 2. Begin integrity repair
    let repair_op_id = "00000000-0000-0000-0001-000000000002";
    let r_status = store
        .begin_library_integrity(library_id, true, repair_op_id, 2000)
        .unwrap();
    assert_eq!(r_status.state, "planned");

    let r_res = store
        .advance_library_operation(repair_op_id, 2000, 100)
        .unwrap();
    assert_eq!(r_res.state, "succeeded");

    let after_repair_page = store.library_item_page(&query).unwrap();
    let after_repair = after_repair_page.items[0].clone();
    assert_eq!(after_repair, expected_from_original_facts);

    let summary_after_repair = store
        .get_user_library_summary(user_id, library_id, media.id)
        .unwrap()
        .unwrap();
    assert_eq!(summary_after_repair, expected_user_summary);
}

#[test]
fn integrity_batching_and_crash_recovery_preserves_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let store = Store::open(&data_dir).unwrap();
    let (lib, root_id) = create_library(&store, "Movies", MediaKind::Movie, "/movies");
    let library_id: LibraryId = lib.id.parse().unwrap();

    let scope = WriteScope {
        library_id,
        config_revision: 0,
    };

    // Create 4 distinct media items
    for i in 1..=4 {
        let id_str = format!("00000000-0000-0000-0000-00000000000{i}");
        let m = create_media(&store, &id_str, &format!("Movie {i}"));
        let file = make_file(
            library_id,
            root_id.clone(),
            &m,
            &format!("/movies/m{i}.mkv"),
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

    // Tamper media 2 and 4
    let db = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    db.execute(
        "DELETE FROM library_items WHERE media_id IN ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000004')",
        [],
    ).unwrap();
    drop(db);

    let op_id = "00000000-0000-0000-0002-000000000001";
    store
        .begin_library_integrity(library_id, true, op_id, 1000)
        .unwrap();

    // Step 1: Advance batch size 2
    let step1 = store.advance_library_operation(op_id, 1000, 2).unwrap();
    assert_eq!(step1.state, "applying");

    // Cursor progressed
    let view1 = store.library_operation_view(op_id).unwrap().unwrap();
    let cursor_json = view1.cursor.expect("cursor present");
    assert!(cursor_json.get("last_media_id").is_some());

    // Simulate crash and reopen
    drop(store);
    let reopened = Store::open(&data_dir).unwrap();

    // Step 2: Continue from reopened store
    let step2 = reopened.advance_library_operation(op_id, 2000, 2).unwrap();
    assert_eq!(step2.state, "succeeded");

    // Verify all 4 items exist and match original count
    let page = reopened
        .library_item_page(&LibraryQuery {
            library_id,
            user_id: None,
            filter: Default::default(),
            order: store::library_query::BrowseOrder::Title,
            descending: false,
            page: store::library_query::BrowsePage::Offset {
                start: 0,
                limit: 10,
            },
            include_total: true,
        })
        .unwrap();
    assert_eq!(page.items.len(), 4);

    // Repeated advance is idempotent
    let step3 = reopened.advance_library_operation(op_id, 3000, 2).unwrap();
    assert_eq!(step3.state, "succeeded");
}
