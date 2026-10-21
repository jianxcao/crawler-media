use domain::{
    Confidence, EntityScope, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, PhysicalSourceId, QualitySource, TimelineMode,
    TypedItemId, UnitKey, UserId,
};
use store::Store;
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::{
    BoundPlaybackEvent, EventOutcome, PlaybackEventType, PlaybackMutation, ResumeStatus,
};

#[test]
fn clean_start_end_to_end_storage_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    // 1. Clean start: open Store on clean directory
    let store = Store::open(&data_dir).expect("Store::open should succeed on clean dir");
    assert!(data_dir.join("app.db").is_file());
    assert!(data_dir.join("cache.db").is_file());
    assert!(data_dir.join("library.db").is_file());
    assert!(data_dir.join("subscribe.db").is_file());

    // 2. Create Library & Roots
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
        .find(|r| r.library_id == Some(lib.id.clone()))
        .unwrap();

    // 3. Insert Canonical Media
    let media_id: MediaId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let movie = Media {
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
    };
    store.insert_media(&movie).unwrap();

    // 4. Publish File into Library
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let file = LibraryFile {
        file_id: "00000000-0000-0000-0000-000000000011".parse().unwrap(),
        library_id: scope.library_id,
        root_id: root.id,
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
    };

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
        event_type: PlaybackEventType::Progress,
    };
    store.commit_playback_event(&event, 2000).unwrap();

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

#[test]
fn unrelated_library_growth_does_not_expand_seek_work() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let lib1 = store
        .create_library(MediaKind::Movie, "Lib 1", &["/lib1"], "everyone", true, &[])
        .unwrap();
    let lib1_id: LibraryId = lib1.id.parse().unwrap();
    let roots = store.list_library_roots().unwrap();
    let root1 = roots
        .iter()
        .find(|r| r.library_id.as_deref() == Some(&lib1.id))
        .unwrap();

    let lib2 = store
        .create_library(MediaKind::Movie, "Lib 2", &["/lib2"], "everyone", true, &[])
        .unwrap();
    let lib2_id: LibraryId = lib2.id.parse().unwrap();
    let roots2 = store.list_library_roots().unwrap();
    let root2 = roots2
        .iter()
        .find(|r| r.library_id.as_deref() == Some(&lib2.id))
        .unwrap();

    // Populate baseline in Lib 1 (50 items)
    let scope1 = WriteScope {
        library_id: lib1_id,
        config_revision: 0,
    };
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
        let file = LibraryFile {
            file_id: LedgerId::new(),
            library_id: lib1_id,
            root_id: root1.id.clone(),
            media_id: m_id,
            kind: MediaKind::Movie,
            path: format!("/lib1/m{:04}.mkv", i).into(),
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
        };
        store
            .publish_files(
                &scope1,
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
        let file = LibraryFile {
            file_id: LedgerId::new(),
            library_id: lib2_id,
            root_id: root2.id.clone(),
            media_id: m_id,
            kind: MediaKind::Movie,
            path: format!("/lib2/other{:04}.mkv", i).into(),
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
        };
        store
            .publish_files(
                &scope2,
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

    // Re-query Lib 1: page items must be identical, fullscan steps 0, seek work bounded
    let (large_page, large_stats) = store.library_item_page_with_stats(&q1).unwrap();
    let large_page_ids: Vec<_> = large_page.items.iter().map(|item| item.media_id).collect();

    // unrelated_library_growth_does_not_expand_seek_work
    assert_eq!(large_page_ids, baseline_page_ids);
    assert_eq!(large_stats.fullscan_steps, 0);
    assert!(large_stats.vm_steps <= baseline_stats.vm_steps * 2);
}
