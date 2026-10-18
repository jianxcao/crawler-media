use domain::{
    Confidence, EntityScope, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, PhysicalSourceId, QualitySource, TimelineMode, TypedItemId, UnitKey,
    UnitScope, UserId,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::{
    BoundPlaybackEvent, EventOutcome, PlaybackEventType, PlaybackMutation, ResolvedResume,
    ResumeStatus,
};
use store::Store;

fn create_media(store: &Store, id: &str, title: &str, kind: MediaKind) -> Media {
    let m = Media {
        id: id.parse().unwrap(),
        kind,
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
    duration_ms: i64,
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
        size_bytes: Some(1_000_000),
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
        duration_ms: Some(duration_ms),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

#[test]
fn playback_resolve_and_mutation_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id = "00000000-0000-0000-0000-000000000001";
    create_media(&store, media_id, "Show", MediaKind::Tv);

    let fid = "00000000-0000-0000-0000-000000000011";
    let file = make_file(fid, &lib.id, &root_id, media_id, "Show/S01E01.mkv", MediaKind::Tv, 2_400_000);
    store.publish_files(&scope, &[StorePublication {
        file: &file,
        units: &[UnitKey::new(1, 1)],
        expected_version: None,
        probe_required: false,
    }], 1000).unwrap();

    let user_id: UserId = "00000000-0000-0000-0000-000000000099".parse().unwrap();
    let item_id = TypedItemId::Unit {
        media: media_id.parse().unwrap(),
        key: UnitKey::new(1, 1),
    };

    // 1. Initial resolve: NoProgress
    let res = store.resolve_playback(user_id, &item_id, None, None).expect("resolve_playback");
    assert_eq!(res.status, ResumeStatus::NoProgress);
    assert_eq!(res.start_ms, 0);

    // 2. Commit a Progress playback event at 120,000ms (2 minutes)
    let event = BoundPlaybackEvent {
        user_id,
        device_id: "device_web".into(),
        session_id: "sess_1".into(),
        generation: 1,
        event_sequence: 1,
        item: item_id.clone(),
        source_id: PhysicalSourceId { file_id: file.file_id, revision: 1 },
        source_version: "v1".into(),
        position_ms: 120_000,
        duration_ms: Some(2_400_000),
        audio_track: None,
        subtitle_track: None,
        paused: false,
        rate_bps: 0,
        client: Some("Web".into()),
        event_type: PlaybackEventType::Progress,
    };

    let outcome = store.commit_playback_event(&event, 1000).expect("commit_playback_event");
    assert_eq!(outcome, EventOutcome::Applied);

    // 3. Resolve again: Compatible, starts at 120,000ms
    let res2 = store.resolve_playback(user_id, &item_id, None, None).expect("resolve_playback");
    assert_eq!(res2.status, ResumeStatus::Compatible);
    assert_eq!(res2.start_ms, 120_000);

    // 4. Mark played via mutation
    let mutation = PlaybackMutation {
        user_id,
        scope: UnitScope::Episode { season: 1, episode: 1 },
        played: Some(true),
        favorite: Some(true),
        expected_generation: None,
    };
    store.mutate_playback(media_id.parse().unwrap(), &mutation, 2000).expect("mutate_playback");

    // 5. Resolve when played: AlreadyPlayed
    let res3 = store.resolve_playback(user_id, &item_id, None, None).expect("resolve_playback");
    assert_eq!(res3.status, ResumeStatus::AlreadyPlayed);
}

#[test]
fn playback_source_change_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let (lib, root_id) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let media_id = "00000000-0000-0000-0000-000000000001";
    create_media(&store, media_id, "Show", MediaKind::Tv);

    let fid1 = "00000000-0000-0000-0000-000000000011";
    let file1 = make_file(fid1, &lib.id, &root_id, media_id, "Show/S01E01.v1.mkv", MediaKind::Tv, 2_400_000);
    store.publish_files(&scope, &[StorePublication {
        file: &file1,
        units: &[UnitKey::new(1, 1)],
        expected_version: None,
        probe_required: false,
    }], 1000).unwrap();

    let user_id: UserId = "00000000-0000-0000-0000-000000000099".parse().unwrap();
    let item_id = TypedItemId::Unit {
        media: media_id.parse().unwrap(),
        key: UnitKey::new(1, 1),
    };

    // Commit progress against file1
    let event = BoundPlaybackEvent {
        user_id,
        device_id: "device_web".into(),
        session_id: "sess_1".into(),
        generation: 1,
        event_sequence: 1,
        item: item_id.clone(),
        source_id: PhysicalSourceId { file_id: file1.file_id, revision: 1 },
        source_version: "v1".into(),
        position_ms: 500_000,
        duration_ms: Some(2_400_000),
        audio_track: None,
        subtitle_track: None,
        paused: false,
        rate_bps: 0,
        client: Some("Web".into()),
        event_type: PlaybackEventType::Progress,
    };
    store.commit_playback_event(&event, 1000).unwrap();

    // Now publish a newer version of the file (e.g. replace or upgraded release)
    let fid2 = "00000000-0000-0000-0000-000000000022";
    let mut file2 = make_file(fid2, &lib.id, &root_id, media_id, "Show/S01E01.v2.mkv", MediaKind::Tv, 2_400_000);
    file2.version.revision = 2;
    store.publish_files(&scope, &[StorePublication {
        file: &file2,
        units: &[UnitKey::new(1, 1)],
        expected_version: None,
        probe_required: false,
    }], 1500).unwrap();

    // Resolve specifying the preferred source file2 -> Status must detect SourceChanged!
    let res = store.resolve_playback(user_id, &item_id, Some(&PhysicalSourceId { file_id: file2.file_id, revision: 2 }), None).unwrap();
    assert_eq!(res.status, ResumeStatus::SourceChanged);
    assert_eq!(res.start_ms, 0, "Source changed must reset start_ms to 0");
}
