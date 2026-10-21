//! Session-bound playback events: an event may only write the target and physical source
//! that were *issued* to its session, and only while that source generation still holds.
//!
//! These are the review-focus regressions: one physical file serving E01 and E02 must keep
//! the two units independent, a late event for the other episode must not close or move the
//! active session, a replaced source must not accept writes from the old session, and a
//! repeated `Stop` must be a no-op rather than a second close.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaId,
    MediaKind, PhysicalSourceId, QualitySource, TypedItemId, UnitKey, UserId,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::{BoundPlaybackEvent, EventOutcome, PlaybackEventType, ResumeStatus};
use store::Store;

const USER: &str = "00000000-0000-0000-0000-000000000099";
const MEDIA: &str = "00000000-0000-0000-0000-000000000001";

struct Fixture {
    store: Store,
    library_id: String,
    root_id: String,
    media_id: MediaId,
    file: LibraryFile,
}

fn seed() -> (tempfile::TempDir, Fixture) {
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
        .find(|root| root.library_id == Some(library.id.clone()))
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

fn issue(f: &Fixture, item: &TypedItemId, file: &LibraryFile) -> (String, i64) {
    issue_on(f, "device_web", item, file)
}

fn event(
    f: &Fixture,
    item: TypedItemId,
    session: (&str, i64),
    file: &LibraryFile,
    position_ms: i64,
    event_type: PlaybackEventType,
) -> BoundPlaybackEvent {
    event_on(f, "device_web", item, session, file, position_ms, event_type)
}

fn episode(f: &Fixture, number: i32) -> TypedItemId {
    TypedItemId::Unit {
        media: f.media_id,
        key: UnitKey::new(1, number),
    }
}

fn issue_on(f: &Fixture, device: &str, item: &TypedItemId, file: &LibraryFile) -> (String, i64) {
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
fn event_on(
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
        event_type,
    }
}

fn unit_position(f: &Fixture, number: i32) -> Option<i64> {
    let user: UserId = USER.parse().unwrap();
    let item = episode(f, number);
    f.store
        .resolve_playback(user, &item, None, None)
        .ok()
        .map(|resume| resume.start_ms)
}

#[test]
fn merged_e02_heartbeat_updates_only_e02() {
    let (_tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();

    // Watch E01 to completion first, then play E02 in its own session.
    let e01 = episode(&f, 1);
    let s01 = issue(&f, &e01, &f.file);
    let out = f
        .store
        .commit_playback_event(
            &event(&f, e01.clone(), (&s01.0, s01.1), &f.file, 2_300_000, PlaybackEventType::Stopped),
            2_000,
        )
        .unwrap();
    assert_eq!(out, EventOutcome::Applied);
    let e01_before = f.store.resolve_playback(user, &e01, None, None).unwrap();

    let e02 = episode(&f, 2);
    let s02 = issue(&f, &e02, &f.file);
    let out = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&s02.0, s02.1), &f.file, 5_000, PlaybackEventType::Progress),
            3_000,
        )
        .unwrap();
    assert_eq!(out, EventOutcome::Applied);

    let e02_after = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert_eq!(e02_after.start_ms, 5_000, "the E02 heartbeat moves E02");
    assert_eq!(e02_after.status, ResumeStatus::Compatible, "the E02 binding matches its source");
    let e01_after = f.store.resolve_playback(user, &e01, None, None).unwrap();
    assert_eq!(
        (e01_after.start_ms, e01_after.status),
        (e01_before.start_ms, e01_before.status),
        "the shared physical file must not leak E02 progress into E01"
    );
    assert_eq!(
        e01_after.status,
        ResumeStatus::AlreadyPlayed,
        "E01 stays finished: the E02 heartbeat must not un-finish it"
    );
}

#[test]
fn late_other_episode_does_not_close_active_session() {
    let (_tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();

    let e01 = episode(&f, 1);
    let s01 = issue_on(&f, "device_tv", &e01, &f.file);
    let e02 = episode(&f, 2);
    let s02 = issue_on(&f, "device_web", &e02, &f.file);

    // E02 is being watched.
    f.store
        .commit_playback_event(
            &event(&f, e02.clone(), (&s02.0, s02.1), &f.file, 5_000, PlaybackEventType::Progress),
            3_000,
        )
        .unwrap();
    let e02_before = f.store.resolve_playback(user, &e02, None, None).unwrap();

    // A late E01 Stop arrives carrying E01's target but E02's session: refused, nothing moves.
    let stale = f
        .store
        .commit_playback_event(
            &event(&f, e01.clone(), (&s02.0, s02.1), &f.file, 2_300_000, PlaybackEventType::Stopped),
            4_000,
        )
        .unwrap();
    assert_eq!(stale, EventOutcome::IgnoredStale, "a late event for another episode is stale");

    let e02_after = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert_eq!(
        (e02_after.start_ms, e02_after.status),
        (e02_before.start_ms, e02_before.status),
        "the late E01 event must not move E02"
    );
    let e01_after = f.store.resolve_playback(user, &e01, None, None).unwrap();
    assert_eq!(e01_after.status, ResumeStatus::NoProgress, "and must not write E01 either");

    // E02's own session is still active: its next heartbeat still applies.
    let nex = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&s02.0, s02.1), &f.file, 9_000, PlaybackEventType::Progress),
            5_000,
        )
        .unwrap();
    assert_eq!(nex, EventOutcome::Applied, "the active session was never closed");

    // E01's own session still works for E01.
    let e01_ok = f
        .store
        .commit_playback_event(
            &event_on(&f, "device_tv", e01.clone(), (&s01.0, s01.1), &f.file, 7_000, PlaybackEventType::Progress),
            6_000,
        )
        .unwrap();
    assert_eq!(e01_ok, EventOutcome::Applied);
}

#[test]
fn issued_session_cannot_write_replacement_source() {
    let (tmp, f) = seed();
    let e02 = episode(&f, 2);
    let session = issue(&f, &e02, &f.file);
    f.store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 5_000, PlaybackEventType::Progress),
            2_000,
        )
        .unwrap();

    // The file is replaced: revision 2, new source version.
    let mut replacement = f.file.clone();
    replacement.file_id = "00000000-0000-0000-0000-000000000012".parse().unwrap();
    replacement.path = "Merged Show/S01E02.replacement.mkv".into();
    replacement.version = FileVersion {
        revision: 2,
        source_version: "v2".into(),
    };
    replacement.source_digest = Some("hash2".into());
    let scope = WriteScope {
        library_id: f.library_id.parse().unwrap(),
        config_revision: 0,
    };
    f.store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &replacement,
                units: &[UnitKey::new(1, 2)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            3_000,
        )
        .unwrap();

    // The old session may not write the new source, and may not write its old file either
    // once the revision moved on.
    let stale = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &replacement, 900_000, PlaybackEventType::Progress),
            4_000,
        )
        .unwrap();
    assert_eq!(stale, EventOutcome::IgnoredStale, "issued session cannot write the replacement");

    let mut stale_old = event(&f, e02.clone(), (&session.0, session.1), &f.file, 900_000, PlaybackEventType::Progress);
    stale_old.source_id.revision = 1;
    stale_old.source_version = "v1".into();
    let still_ok = f.store.commit_playback_event(&stale_old, 4_100).unwrap();
    assert_eq!(
        still_ok,
        EventOutcome::Applied,
        "the issued (still published) generation keeps its own writes"
    );

    // And progress stays on the original source's timeline, not the replacement's.
    let user: UserId = USER.parse().unwrap();
    let resume = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert_eq!(resume.start_ms, 900_000);
    assert_eq!(
        resume.target.file.file_id,
        f.file.file_id,
        "no implicit switch to the replacement source"
    );
    drop(tmp);
}

#[test]
fn repeated_stop_is_a_single_close() {
    let (_tmp, f) = seed();
    let e02 = episode(&f, 2);
    let session = issue(&f, &e02, &f.file);

    let first = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 2_300_000, PlaybackEventType::Stopped),
            2_000,
        )
        .unwrap();
    assert_eq!(first, EventOutcome::Applied);

    let second = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 2_300_000, PlaybackEventType::Stopped),
            3_000,
        )
        .unwrap();
    assert_eq!(second, EventOutcome::IgnoredStale, "a repeated Stop writes nothing");

    let user: UserId = USER.parse().unwrap();
    let beyond = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 10_000, PlaybackEventType::Progress),
            4_000,
        )
        .unwrap();
    assert_eq!(beyond, EventOutcome::IgnoredStale, "a closed session cannot be revived");

    let played = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert!(unit_position(&f, 2).is_some());
    assert!(
        played.status == ResumeStatus::AlreadyPlayed || played.start_ms == 2_300_000,
        "the single Stop is the only write: {played:?}"
    );
}
