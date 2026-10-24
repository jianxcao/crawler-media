use domain::{
    Confidence, EntityScope, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, PhysicalSourceId, QualitySource, TimelineMode,
    TypedItemId, UnitKey, UserId,
};
use std::path::Path;
use store::Store;
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::{
    BoundPlaybackEvent, EventOutcome, PlaybackEventType, PlaybackMutation, ResumeStatus,
};

#[path = "storage_work_bounds/scan_windows.rs"]
mod scan_windows;
#[path = "storage_work_bounds/scan_windows_measurement.rs"]
mod scan_windows_measurement;
#[path = "storage_work_bounds/seek_scale.rs"]
mod seek_scale;

#[test]
fn clean_start_end_to_end_storage_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    // 1. Clean start: open Store on clean directory
    let store = open_clean_start_store(&data_dir);

    // 2. Create Library & Roots
    let (library_id, root_id) = create_action_movies_library(&store);

    // 3. Insert Canonical Media
    let media_id: MediaId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let movie = matrix_media(media_id);
    store.insert_media(&movie).unwrap();

    // 4. Publish File into Library
    let scope = WriteScope {
        library_id,
        config_revision: 0,
    };
    let file = matrix_file(
        "00000000-0000-0000-0000-000000000011".parse().unwrap(),
        library_id,
        &root_id,
        media_id,
    );
    publish_whole_file(&store, &scope, &file);

    // 5. Query Library Item Page (Single-table index read)
    let q = LibraryQuery {
        library_id: scope.library_id,
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
    let page = store.library_item_page(&q).unwrap();
    assert_eq!(page.total, Some(1));
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].title, "The Matrix");

    // 6. Playback Resume and Tracking
    let user_id: UserId = "00000000-0000-0000-0000-000000000099".parse().unwrap();
    let item_id = TypedItemId::Work(media_id);
    // A playback event is only accepted for a session issued for this target and source.
    let (session_id, generation) = issue_progress_session(&store, user_id, &file, media_id);
    commit_progress_event(&store, user_id, session_id, generation, &item_id, &file);

    let resume = store
        .resolve_playback(user_id, &item_id, None, None)
        .unwrap();
    assert_eq!(resume.status, ResumeStatus::Compatible);
    assert_eq!(resume.start_ms, 1_200_000);

    // 7. Restart and Reopen: All facts remain fully intact
    drop(store);
    let store2 = Store::open(&data_dir).expect("Idempotent reopen");
    let page2 = store2.library_item_page(&q).unwrap();
    assert_eq!(page2.total, Some(1));
    assert_eq!(page2.items[0].title, "The Matrix");

    let resume2 = store2
        .resolve_playback(user_id, &item_id, None, None)
        .unwrap();
    assert_eq!(resume2.status, ResumeStatus::Compatible);
    assert_eq!(resume2.start_ms, 1_200_000);
}

// Fixture helpers for the clean-start lifecycle test.

/// Opens a Store on a clean directory, checks the four database files, returns the Store.
fn open_clean_start_store(data_dir: &Path) -> Store {
    let store = Store::open(data_dir).expect("Store::open should succeed on clean dir");
    assert!(data_dir.join("app.db").is_file());
    assert!(data_dir.join("cache.db").is_file());
    assert!(data_dir.join("library.db").is_file());
    assert!(data_dir.join("subscribe.db").is_file());
    store
}

/// Creates the "Action Movies" Library; returns (library id, primary root id).
fn create_action_movies_library(store: &Store) -> (LibraryId, String) {
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Action Movies",
            &["/movies"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots
        .into_iter()
        .find(|r| r.library_id == lib.id.clone())
        .unwrap();
    (lib.id.parse().unwrap(), root.id)
}

/// The canonical Movie inserted by the clean-start lifecycle test.
fn matrix_media(media_id: MediaId) -> Media {
    Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    }
}

/// The single published file of the clean-start lifecycle test.
fn matrix_file(
    file_id: LedgerId,
    library_id: LibraryId,
    root_id: &str,
    media_id: MediaId,
) -> LibraryFile {
    LibraryFile {
        file_id,
        library_id,
        root_id: root_id.to_string(),
        media_id,
        kind: MediaKind::Movie,
        path: "The.Matrix.1999.mkv".into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(4_000_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash1".into()),
        source_path: None,
        transfer_mode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: Some("HDR10".into()),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some("2160p UHD".into()),
        duration_ms: Some(8_160_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

/// Publishes one file as a whole-file unit (shared by every fixture in this file).
fn publish_whole_file(store: &Store, scope: &WriteScope, file: &LibraryFile) {
    store
        .publish_files(
            scope,
            &[StorePublication {
                file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();
}

/// Issues a play session for the whole file and returns (session id, generation).
fn issue_progress_session(
    store: &Store,
    user_id: UserId,
    file: &LibraryFile,
    media_id: MediaId,
) -> (String, i64) {
    let session_id = store
        .issue_play_session(
            user_id,
            "tv_client",
            file.file_id,
            media_id,
            domain::UnitKey::WHOLE,
            &file.version,
            2_000,
        )
        .expect("issue play session");
    let generation = store
        .play_session_binding(user_id, "tv_client", &session_id)
        .expect("binding")
        .expect("issued binding")
        .generation;
    (session_id, generation)
}

/// Commits one Progress event for that session.
fn commit_progress_event(
    store: &Store,
    user_id: UserId,
    session_id: String,
    generation: i64,
    item_id: &TypedItemId,
    file: &LibraryFile,
) {
    let event = BoundPlaybackEvent {
        user_id,
        device_id: "tv_client".into(),
        session_id,
        generation,
        event_sequence: Some(1),
        item: item_id.clone(),
        source_id: PhysicalSourceId {
            file_id: file.file_id,
            revision: 1,
        },
        source_version: "v1".into(),
        position_ms: 1_200_000,
        duration_ms: Some(8_160_000),
        audio_track: None,
        subtitle_track: None,
        paused: false,
        rate_bps: 0,
        client: Some("AppleTV".into()),
        device_name: None,
        client_version: None,
        play_method: None,
        event_type: PlaybackEventType::Progress,
    };
    store.commit_playback_event(&event, 2000).unwrap();
}

#[test]
fn unrelated_library_growth_does_not_expand_seek_work() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let (lib1_id, root1_id) = create_named_library(&store, "Lib 1", "/lib1");
    let (lib2_id, root2_id) = create_named_library(&store, "Lib 2", "/lib2");

    // Populate baseline in Lib 1 (50 items)
    let scope1 = WriteScope {
        library_id: lib1_id,
        config_revision: 0,
    };
    seed_baseline_movies(&store, &scope1, &root1_id);

    let q1 = LibraryQuery {
        library_id: lib1_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: false,
    };
    let (baseline_page, baseline_stats) = store.library_item_page_with_stats(&q1).unwrap();
    let baseline_page_ids: Vec<_> = baseline_page
        .items
        .iter()
        .map(|item| item.media_id)
        .collect();

    // Now populate 500 items in unrelated Lib 2
    let scope2 = WriteScope {
        library_id: lib2_id,
        config_revision: 0,
    };
    seed_unrelated_movies(&store, &scope2, &root2_id);

    // Re-query Lib 1: page items must be identical, fullscan steps 0, seek work bounded
    let (large_page, large_stats) = store.library_item_page_with_stats(&q1).unwrap();
    let large_page_ids: Vec<_> = large_page.items.iter().map(|item| item.media_id).collect();

    // unrelated_library_growth_does_not_expand_seek_work
    assert_eq!(large_page_ids, baseline_page_ids);
    assert_eq!(large_stats.fullscan_steps, 0);
    assert!(large_stats.vm_steps <= baseline_stats.vm_steps * 2);
}

// Fixture helpers for the unrelated-library-growth test.

/// Creates a movie Library with one root; returns (library id, root id).
fn create_named_library(store: &Store, name: &str, root_path: &str) -> (LibraryId, String) {
    let lib = store
        .create_library(MediaKind::Movie, name, &[root_path], "everyone", true, &[])
        .unwrap();
    let lib_id: LibraryId = lib.id.parse().unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots
        .iter()
        .find(|r| r.library_id.as_str() == lib.id.as_str())
        .unwrap();
    (lib_id, root.id.clone())
}

/// A minimal whole-file movie row: 1 KiB, no probe facts, one unit.
fn plain_movie_file(
    file_id: LedgerId,
    library_id: LibraryId,
    root_id: &str,
    media_id: MediaId,
    path: String,
) -> LibraryFile {
    LibraryFile {
        file_id,
        library_id,
        root_id: root_id.to_string(),
        media_id,
        kind: MediaKind::Movie,
        path: path.into(),
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

/// Populates the baseline: 50 items in Lib 1.
fn seed_baseline_movies(store: &Store, scope: &WriteScope, root_id: &str) {
    for i in 0..50 {
        let m_id: MediaId = format!("00000000-0000-0000-0001-{:012x}", i)
            .parse()
            .unwrap();
        let m = Media {
            id: m_id,
            kind: MediaKind::Movie,
            title: format!("Movie {:04}", i),
            year: Some(2000 + (i as u16 % 20)),
            original_title: None,
            tmdb_id: Some(format!("tmdb-1-{i}")),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        };
        store.insert_media(&m).unwrap();
        let file = plain_movie_file(
            LedgerId::new(),
            scope.library_id,
            root_id,
            m_id,
            format!("/lib1/m{:04}.mkv", i),
        );
        publish_whole_file(store, scope, &file);
    }
}

/// Populates the unrelated library: 500 items in Lib 2.
fn seed_unrelated_movies(store: &Store, scope: &WriteScope, root_id: &str) {
    for i in 0..500 {
        let m_id: MediaId = format!("00000000-0000-0000-0002-{:012x}", i)
            .parse()
            .unwrap();
        let m = Media {
            id: m_id,
            kind: MediaKind::Movie,
            title: format!("Other Movie {:04}", i),
            year: Some(2000),
            original_title: None,
            tmdb_id: Some(format!("tmdb-2-{i}")),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        };
        store.insert_media(&m).unwrap();
        let file = plain_movie_file(
            LedgerId::new(),
            scope.library_id,
            root_id,
            m_id,
            format!("/lib2/other{:04}.mkv", i),
        );
        publish_whole_file(store, scope, &file);
    }
}
