//! Source-aware resume (design §4.5 R1): `Store::resolve_playback` reuses a
//! stored position only while the whole binding (file, revision, source
//! version, timeline mode, tracks version) still describes the selected source.
//!
//! Every fixture is seeded through the public store API so publication,
//! presence and track revisions are the real ones the resolver reads.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaKind,
    PhysicalSourceId, QualitySource, TimelineMode, TypedItemId, UnitKey, UserId,
};
use store::library_write::probe::ProbePublication;
use store::library_write::{
    FilePublication as StorePublication, PresenceMode, PresenceObservation, WriteScope,
};
use store::playback::{BoundPlaybackEvent, EventOutcome, PlaybackEventType, ResumeStatus};
use store::Store;

const USER: &str = "00000000-0000-0000-0000-000000000099";
const MEDIA: &str = "00000000-0000-0000-0000-000000000001";

struct Fixture {
    store: Store,
    root_id: String,
    media_id: domain::MediaId,
    scope: WriteScope,
    _tmp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let lib = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(lib.id.as_str()))
        .unwrap();
    let media_id: domain::MediaId = MEDIA.parse().unwrap();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Show".into(),
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
    Fixture {
        scope: WriteScope {
            library_id: lib.id.parse().unwrap(),
            config_revision: 0,
        },
        store,
        root_id: root.id,
        media_id,
        _tmp: tmp,
    }
}

fn make_file(
    f: &Fixture,
    file_id: &str,
    rel_path: &str,
    duration_ms: i64,
    revision: i64,
    source_version: &str,
    publication: FilePublication,
) -> LibraryFile {
    LibraryFile {
        file_id: file_id.parse().unwrap(),
        library_id: f.scope.library_id,
        root_id: f.root_id.clone(),
        media_id: f.media_id,
        kind: MediaKind::Tv,
        path: rel_path.into(),
        version: FileVersion {
            revision,
            source_version: source_version.to_string(),
        },
        presence: FilePresence::Present,
        publication,
        size_bytes: Some(1_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some(format!("digest-{file_id}")),
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some("1080p BluRay".into()),
        duration_ms: Some(duration_ms),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn publish(f: &Fixture, file: &LibraryFile, units: &[UnitKey]) {
    f.store
        .publish_files(
            &f.scope,
            &[StorePublication {
                file,
                units,
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
}

fn unit(f: &Fixture, season: i32, episode: i32) -> TypedItemId {
    TypedItemId::Unit {
        media: f.media_id,
        key: UnitKey::new(season, episode),
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_progress(
    f: &Fixture,
    item: &TypedItemId,
    file: &LibraryFile,
    position_ms: i64,
    duration_ms: i64,
    audio: Option<&str>,
    subtitle: Option<&str>,
) {
    // Progress is only accepted for a session that was actually issued for this
    // target and source: the commit validates the issued binding.
    let session_id = f
        .store
        .issue_play_session(
            USER.parse().unwrap(),
            "device_web",
            file.file_id,
            f.media_id,
            domain::UnitKey::new(unit_coords(item).0, unit_coords(item).1),
            &file.version,
            1_000,
        )
        .unwrap();
    let generation = f
        .store
        .play_session_binding(USER.parse().unwrap(), "device_web", &session_id)
        .unwrap()
        .expect("issued binding")
        .generation;
    let outcome = f
        .store
        .commit_playback_event(
            &BoundPlaybackEvent {
                user_id: USER.parse().unwrap(),
                device_id: "device_web".into(),
                session_id,
                generation,
                event_sequence: Some(1),
                item: item.clone(),
                source_id: PhysicalSourceId {
                    file_id: file.file_id,
                    revision: file.version.revision,
                },
                source_version: file.version.source_version.clone(),
                position_ms,
                duration_ms: Some(duration_ms),
                audio_track: audio.map(Into::into),
                subtitle_track: subtitle.map(Into::into),
                paused: false,
                rate_bps: 0,
                client: Some("Web".into()),
                device_name: None,
                client_version: None,
                play_method: None,
                event_type: PlaybackEventType::Progress,
            },
            1_000,
        )
        .unwrap();
    assert_eq!(outcome, EventOutcome::Applied, "an issued session applies its own event");
}

/// `(season, episode)` of a logical item, mirroring the store's own mapping.
fn unit_coords(item: &TypedItemId) -> (i32, i32) {
    match item {
        TypedItemId::Work(_) => (-1, -1),
        TypedItemId::Season { season, .. } => (*season as i32, 1),
        TypedItemId::Unit { key, .. } => (key.season, key.episode),
    }
}

fn user() -> UserId {
    USER.parse().unwrap()
}

/// A merged file covering E01+E02 is one physical source, but each logical
/// unit keeps its own resume position.
#[test]
fn merged_file_keeps_independent_progress_per_unit() {
    let f = fixture();
    let file = make_file(
        &f,
        "00000000-0000-0000-0000-000000000011",
        "Show/S01E01-E02.mkv",
        2_700_000,
        1,
        "v1",
        FilePublication::Published,
    );
    publish(&f, &file, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);

    let e01 = unit(&f, 1, 1);
    let e02 = unit(&f, 1, 2);

    let fresh = f.store.resolve_playback(user(), &e02, None, None).unwrap();
    assert_eq!(fresh.status, ResumeStatus::NoProgress);
    assert_eq!(fresh.start_ms, 0);

    commit_progress(&f, &e01, &file, 600_000, 2_700_000, Some("eng"), None);

    let other = f.store.resolve_playback(user(), &e02, None, None).unwrap();
    assert_eq!(
        other.status,
        ResumeStatus::NoProgress,
        "progress on E01 must not leak into E02"
    );
    assert_eq!(other.start_ms, 0);
    assert_eq!(other.target.file.file_id, file.file_id);
    assert_eq!(other.target.unit, UnitKey::new(1, 2));

    let same = f.store.resolve_playback(user(), &e01, None, None).unwrap();
    assert_eq!(same.status, ResumeStatus::Compatible);
    assert_eq!(same.start_ms, 600_000);
    assert_eq!(same.target.file.file_id, file.file_id);
    assert_eq!(same.target.unit, UnitKey::new(1, 1));
}

/// Replacing the source (a new revision/source version of the same file)
/// resets the position and the track selection to the new source's facts.
#[test]
fn replaced_source_resets_position_and_tracks() {
    let f = fixture();
    let file = make_file(
        &f,
        "00000000-0000-0000-0000-000000000021",
        "Show/S01E01.mkv",
        5_400_000,
        1,
        "v1",
        FilePublication::Published,
    );
    publish(&f, &file, &[UnitKey::new(1, 1)]);
    let e01 = unit(&f, 1, 1);

    commit_progress(&f, &e01, &file, 900_000, 5_400_000, Some("eng"), Some("off"));
    let bound = f.store.resolve_playback(user(), &e01, None, None).unwrap();
    assert_eq!(bound.status, ResumeStatus::Compatible);
    assert_eq!(bound.start_ms, 900_000);
    assert_eq!(bound.audio_track.as_deref(), Some("eng"));

    let replacement = make_file(
        &f,
        "00000000-0000-0000-0000-000000000021",
        "Show/S01E01.mkv",
        7_200_000,
        2,
        "v2",
        FilePublication::Published,
    );
    publish(&f, &replacement, &[UnitKey::new(1, 1)]);

    let res = f.store.resolve_playback(user(), &e01, None, None).unwrap();
    assert_eq!(res.status, ResumeStatus::SourceChanged);
    assert_eq!(res.start_ms, 0, "a changed source never keeps the old position");
    assert_eq!(res.duration_ms, Some(7_200_000), "duration follows the new source");
    assert_eq!(res.audio_track, None, "old audio selection must not be replayed");
    assert_eq!(res.subtitle_track, None, "old subtitle selection must not be replayed");
    assert_eq!(res.target.file.file_id, file.file_id);
    assert_eq!(res.target.file.version.revision, 2);
    assert_eq!(res.target.file.version.source_version, "v2");
}

/// A probe result that advances `tracks_version` invalidates the stored track
/// selection even though file, revision and source version are unchanged.
#[test]
fn tracks_version_bump_resets_track_selection() {
    let f = fixture();
    let file = make_file(
        &f,
        "00000000-0000-0000-0000-000000000031",
        "Show/S01E01.mkv",
        5_400_000,
        1,
        "v1",
        FilePublication::Published,
    );
    publish(&f, &file, &[UnitKey::new(1, 1)]);
    let e01 = unit(&f, 1, 1);

    commit_progress(&f, &e01, &file, 300_000, 5_400_000, Some("eng"), Some("forced"));
    let bound = f.store.resolve_playback(user(), &e01, None, None).unwrap();
    assert_eq!(bound.status, ResumeStatus::Compatible);
    assert_eq!(bound.audio_track.as_deref(), Some("eng"));
    assert_eq!(bound.subtitle_track.as_deref(), Some("forced"));

    let version = f.store.get_file_version(file.file_id).unwrap().unwrap();
    let published = f
        .store
        .publish_probe_result(
            file.file_id,
            &ProbePublication {
                expected: &version,
                tracks_version: 2,
                resolution: None,
                codec: None,
                hdr: None,
                duration_ms: None,
                details: None,
            },
            1_500,
        )
        .unwrap();
    assert!(published, "the probe result must be accepted");

    let res = f.store.resolve_playback(user(), &e01, None, None).unwrap();
    // R13: re-probing tracks of the same bytes keeps the timeline, only tracks reset.
    assert_eq!(res.status, ResumeStatus::Compatible);
    assert_eq!(res.start_ms, 300_000, "same-source track refresh keeps the position");
    assert_eq!(res.duration_ms, Some(5_400_000));
    assert_eq!(res.audio_track, None);
    assert_eq!(res.subtitle_track, None);
    assert_eq!(res.target.file.version.revision, 1, "the file itself did not change");
    assert_eq!(res.target.timeline, TimelineMode::FullFile);
}

/// A client-supplied position is honoured whether or not a binding exists.
#[test]
fn explicit_start_position_wins() {
    let f = fixture();
    let file = make_file(
        &f,
        "00000000-0000-0000-0000-000000000041",
        "Show/S01E01.mkv",
        5_400_000,
        1,
        "v1",
        FilePublication::Published,
    );
    publish(&f, &file, &[UnitKey::new(1, 1)]);
    let e01 = unit(&f, 1, 1);

    let no_binding = f
        .store
        .resolve_playback(user(), &e01, None, Some(42_000))
        .unwrap();
    assert_eq!(no_binding.start_ms, 42_000);

    commit_progress(&f, &e01, &file, 300_000, 5_400_000, None, None);
    let explicit = f
        .store
        .resolve_playback(user(), &e01, None, Some(600_000))
        .unwrap();
    assert_eq!(
        explicit.start_ms, 600_000,
        "an explicit start must win over the stored 300_000 ms"
    );
    assert_eq!(explicit.duration_ms, Some(5_400_000));

    let stored = f.store.resolve_playback(user(), &e01, None, None).unwrap();
    assert_eq!(stored.start_ms, 300_000);
}

/// Pending and missing files are management facts, never playable sources.
#[test]
fn pending_and_missing_files_are_never_playable() {
    let f = fixture();
    let pending = make_file(
        &f,
        "00000000-0000-0000-0000-000000000051",
        "Show/S01E01.mkv",
        2_700_000,
        1,
        "v1",
        FilePublication::Pending,
    );
    publish(&f, &pending, &[UnitKey::new(1, 1)]);

    let pending_err = f
        .store
        .resolve_playback(user(), &unit(&f, 1, 1), None, None);
    assert!(
        pending_err.is_err(),
        "a pending file must not resolve as a playable source"
    );

    let missing = make_file(
        &f,
        "00000000-0000-0000-0000-000000000052",
        "Show/S01E02.mkv",
        2_700_000,
        1,
        "v1",
        FilePublication::Published,
    );
    publish(&f, &missing, &[UnitKey::new(1, 2)]);
    let e02 = unit(&f, 1, 2);
    assert!(f.store.resolve_playback(user(), &e02, None, None).is_ok());

    f.store
        .apply_presence(
            &f.scope,
            &[PresenceObservation {
                file_id: missing.file_id,
                observed_version: missing.version.clone(),
                observed_path: missing.path.clone(),
                presence: FilePresence::Missing,
            }],
            PresenceMode::Verify,
            1_500,
        )
        .unwrap();

    let missing_err = f.store.resolve_playback(user(), &e02, None, None);
    assert!(
        missing_err.is_err(),
        "a missing file must not resolve as a playable source"
    );
}
