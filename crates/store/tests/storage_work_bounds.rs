use domain::{
    Confidence, EntityScope, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, PhysicalSourceId, QualitySource, TimelineMode, TypedItemId,
    UnitKey, UserId,
};
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::{
    BoundPlaybackEvent, EventOutcome, PlaybackEventType, PlaybackMutation, ResumeStatus,
};
use store::Store;

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
        .create_library(MediaKind::Movie, "Action Movies", &["/movies"], "everyone", true, &[])
        .unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots.into_iter().find(|r| r.library_id == Some(lib.id.clone())).unwrap();

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
        version: FileVersion { revision: 1, source_version: "v1".into() },
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

    store.publish_files(&scope, &[StorePublication {
        file: &file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        probe_required: false,
    }], 1000).unwrap();

    // 5. Query Library Item Page (Single-table index read)
    let q = LibraryQuery {
        library_id: scope.library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 0, limit: 10 },
        include_total: true,
    };
    let page = store.library_item_page(&q).unwrap();
    assert_eq!(page.total, Some(1));
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].title, "The Matrix");

    // 6. Playback Resume and Tracking
    let user_id: UserId = "00000000-0000-0000-0000-000000000099".parse().unwrap();
    let item_id = TypedItemId::Work(media_id);
    let event = BoundPlaybackEvent {
        user_id,
        device_id: "tv_client".into(),
        session_id: "session_1".into(),
        generation: 1,
        event_sequence: 1,
        item: item_id.clone(),
        source_id: PhysicalSourceId { file_id: file.file_id, revision: 1 },
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

    let resume = store.resolve_playback(user_id, &item_id, None, None).unwrap();
    assert_eq!(resume.status, ResumeStatus::Compatible);
    assert_eq!(resume.start_ms, 1_200_000);

    // 7. Restart and Reopen: All facts remain fully intact
    drop(store);
    let store2 = Store::open(&data_dir).expect("Idempotent reopen");
    let page2 = store2.library_item_page(&q).unwrap();
    assert_eq!(page2.total, Some(1));
    assert_eq!(page2.items[0].title, "The Matrix");

    let resume2 = store2.resolve_playback(user_id, &item_id, None, None).unwrap();
    assert_eq!(resume2.status, ResumeStatus::Compatible);
    assert_eq!(resume2.start_ms, 1_200_000);
}
