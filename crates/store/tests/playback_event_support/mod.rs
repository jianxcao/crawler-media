//! Shared session-bound playback event fixture (one merged E01/E02 file).
#![allow(dead_code, unused_imports)]

pub use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaId, MediaKind,
    PhysicalSourceId, QualitySource, TypedItemId, UnitKey, UserId,
};
pub use store::Store;
pub use store::library_write::{FilePublication as StorePublication, WriteScope};
pub use store::playback::{
    BoundPlaybackEvent, EventOutcome, PlaySessionIdState, PlaybackEventType, ResumeStatus,
};

pub const USER: &str = "00000000-0000-0000-0000-000000000099";
pub const MEDIA: &str = "00000000-0000-0000-0000-000000000001";

pub struct Fixture {
    pub store: Store,
    pub library_id: String,
    pub root_id: String,
    pub media_id: MediaId,
    pub file: LibraryFile,
}

pub fn seed() -> (tempfile::TempDir, Fixture) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media_id: MediaId = MEDIA.parse().unwrap();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Merged Show".into(),
            year: Some(2024),
            original_title: None,
            tmdb_id: Some(MEDIA.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
    let library = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == library.id.clone())
        .unwrap()
        .id;
    let file = LibraryFile {
        file_id: "00000000-0000-0000-0000-000000000011".parse().unwrap(),
        library_id: library.id.parse().unwrap(),
        root_id: root_id.clone(),
        media_id,
        kind: MediaKind::Tv,
        path: "Merged Show/S01E01-E02.mkv".into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(2_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash1".into()),
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(2_400_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    // One physical file covering both E01 and E02.
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    (
        tmp,
        Fixture {
            store,
            library_id: library.id,
            root_id,
            media_id,
            file,
        },
    )
}

pub fn issue(f: &Fixture, item: &TypedItemId, file: &LibraryFile) -> (String, i64) {
    issue_on(f, "device_web", item, file)
}

pub fn event(
    f: &Fixture,
    item: TypedItemId,
    session: (&str, i64),
    file: &LibraryFile,
    position_ms: i64,
    event_type: PlaybackEventType,
) -> BoundPlaybackEvent {
    event_on(
        f,
        "device_web",
        item,
        session,
        file,
        position_ms,
        event_type,
    )
}

pub fn episode(f: &Fixture, number: i32) -> TypedItemId {
    TypedItemId::Unit {
        media: f.media_id,
        key: UnitKey::new(1, number),
    }
}

pub fn issue_on(
    f: &Fixture,
    device: &str,
    item: &TypedItemId,
    file: &LibraryFile,
) -> (String, i64) {
    let user: UserId = USER.parse().unwrap();
    let unit = match item {
        TypedItemId::Unit { key, .. } => *key,
        _ => UnitKey::WHOLE,
    };
    let session_id = f
        .store
        .issue_play_session(
            user,
            device,
            file.file_id,
            f.media_id,
            unit,
            &file.version,
            1_000,
        )
        .expect("issue session");
    let generation = f
        .store
        .play_session_binding(user, device, &session_id)
        .unwrap()
        .expect("binding")
        .generation;
    (session_id, generation)
}

#[allow(clippy::too_many_arguments)]
pub fn event_on(
    f: &Fixture,
    device: &str,
    item: TypedItemId,
    session: (&str, i64),
    file: &LibraryFile,
    position_ms: i64,
    event_type: PlaybackEventType,
) -> BoundPlaybackEvent {
    BoundPlaybackEvent {
        user_id: USER.parse().unwrap(),
        device_id: device.to_string(),
        session_id: session.0.to_string(),
        generation: session.1,
        event_sequence: Some(1),
        item,
        source_id: PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        },
        source_version: file.version.source_version.clone(),
        position_ms,
        duration_ms: Some(2_400_000),
        audio_track: None,
        subtitle_track: None,
        paused: false,
        rate_bps: 0,
        client: Some("Web".into()),
        device_name: None,
        client_version: None,
        play_method: None,
        event_type,
    }
}

pub fn unit_position(f: &Fixture, number: i32) -> Option<i64> {
    let user: UserId = USER.parse().unwrap();
    let item = episode(f, number);
    let resume = f
        .store
        .resolve_playback(user, &item, None, None)
        .expect("a failed read must not look like a missing stored position");
    Some(resume.start_ms)
}
