//! Explicit `played` assertions, their scope precedence, and what clearing history keeps.
//!
//! These are the review-focus regressions: a season mark must be visible for episodes that
//! have no per-episode playback row (and must not leak into another season), a mark must not
//! move the watch-activity clock, and clearing history must drop the resume package and the
//! assertions while leaving the favourite intact.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaId, MediaKind,
    PhysicalSourceId, QualitySource, TypedItemId, UnitKey, UserId,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::{PlaybackEventType, PlayedAssertionScope, ResumeStatus};

const USER: &str = "00000000-0000-0000-0000-000000000099";
const MEDIA: &str = "00000000-0000-0000-0000-000000000001";

struct Fixture {
    store: Store,
    scope: WriteScope,
    media_id: MediaId,
    files: Vec<LibraryFile>,
}

fn seed(episodes: &[(i32, i32)]) -> (tempfile::TempDir, Fixture) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media_id: MediaId = MEDIA.parse().unwrap();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Scoped Show".into(),
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
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let mut files = Vec::new();
    for (index, (season, episode)) in episodes.iter().enumerate() {
        let file = LibraryFile {
            file_id: format!("00000000-0000-0000-0000-0000000001{index:02}")
                .parse()
                .unwrap(),
            library_id: library.id.parse().unwrap(),
            root_id: root_id.clone(),
            media_id,
            kind: MediaKind::Tv,
            path: format!("Scoped Show/S{season:02}E{episode:02}.mkv").into(),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(1_000_000),
            stat_known: true,
            created_ns: Some(100),
            modified_ns: Some(200),
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: Some("1080p".into()),
            codec: Some("hevc".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: Some(2_700_000),
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
                    units: &[UnitKey::new(*season, *episode)],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1_000,
            )
            .unwrap();
        files.push(file);
    }
    (
        tmp,
        Fixture {
            store,
            scope,
            media_id,
            files,
        },
    )
}

fn user() -> UserId {
    USER.parse().unwrap()
}

fn episode(f: &Fixture, season: i32, number: i32) -> TypedItemId {
    TypedItemId::Unit {
        media: f.media_id,
        key: UnitKey::new(season, number),
    }
}

/// Commit real progress so the unit carries a resume package bound to `file`.
fn commit_progress(
    f: &Fixture,
    file: &LibraryFile,
    season: i32,
    number: i32,
    position_ms: i64,
    duration_ms: i64,
) {
    let item = episode(f, season, number);
    let session_id = f
        .store
        .issue_play_session(
            user(),
            "device_web",
            file.file_id,
            f.media_id,
            UnitKey::new(season, number),
            &file.version,
            1_000,
        )
        .unwrap();
    let generation = f
        .store
        .play_session_binding(user(), "device_web", &session_id)
        .unwrap()
        .unwrap()
        .generation;
    let outcome = f
        .store
        .commit_playback_event(
            &store::playback::BoundPlaybackEvent {
                user_id: user(),
                device_id: "device_web".into(),
                session_id,
                generation,
                event_sequence: Some(1),
                item,
                source_id: PhysicalSourceId {
                    file_id: file.file_id,
                    revision: file.version.revision,
                },
                source_version: file.version.source_version.clone(),
                position_ms,
                duration_ms: Some(duration_ms),
                audio_track: Some("aac-2.0".into()),
                subtitle_track: Some("chi".into()),
                paused: false,
                rate_bps: 0,
                client: Some("Web".into()),
                device_name: None,
                client_version: None,
                play_method: None,
                event_type: PlaybackEventType::Progress,
            },
            2_000,
        )
        .unwrap();
    assert_eq!(outcome, store::playback::EventOutcome::Applied);
}

#[test]
fn source_change_drops_old_duration_and_tracks() {
    let (_tmp, f) = seed(&[(1, 1)]);
    // A 90-minute file, watched to 60 minutes.
    let mut long = f.files[0].clone();
    long.duration_ms = Some(5_400_000);
    f.store
        .publish_files(
            &f.scope,
            &[StorePublication {
                file: &long,
                units: &[UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_500,
        )
        .unwrap();
    commit_progress(&f, &long, 1, 1, 3_600_000, 5_400_000);

    let resume = f
        .store
        .resolve_playback(user(), &episode(&f, 1, 1), None, None)
        .unwrap();
    assert_eq!(resume.start_ms, 3_600_000);
    assert_eq!(resume.duration_ms, Some(5_400_000));
    assert_eq!(resume.audio_track.as_deref(), Some("aac-2.0"));

    // A replacement source: 45 minutes of different bytes.
    let mut replacement = long.clone();
    replacement.file_id = "00000000-0000-0000-0000-000000000900".parse().unwrap();
    replacement.path = "Scoped Show/S01E01.replacement.mkv".into();
    replacement.version = FileVersion {
        revision: 2,
        source_version: "v2".into(),
    };
    replacement.duration_ms = Some(2_700_000);
    f.store
        .publish_files(
            &f.scope,
            &[StorePublication {
                file: &replacement,
                units: &[UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            3_000,
        )
        .unwrap();

    // Playing the replacement must not inherit the old position, duration or tracks.
    let changed = f
        .store
        .resolve_playback(
            user(),
            &episode(&f, 1, 1),
            Some(&PhysicalSourceId {
                file_id: replacement.file_id,
                revision: replacement.version.revision,
            }),
            None,
        )
        .unwrap();
    assert_eq!(changed.start_ms, 0, "a different source starts from zero");
    assert_eq!(
        changed.duration_ms,
        Some(2_700_000),
        "the duration comes from the source being played, not the stored one"
    );
    assert_eq!(changed.audio_track, None, "old track selection is invalid");
    assert_eq!(changed.subtitle_track, None);

    // While the original source is still in the library, its own resume stays usable.
    let original = f
        .store
        .resolve_playback(
            user(),
            &episode(&f, 1, 1),
            Some(&PhysicalSourceId {
                file_id: long.file_id,
                revision: long.version.revision,
            }),
            None,
        )
        .unwrap();
    assert_eq!(
        original.start_ms, 3_600_000,
        "the original keeps its own position"
    );
}

#[test]
fn season_mark_without_episode_rows_is_visible() {
    let (_tmp, f) = seed(&[(1, 1), (1, 2), (2, 1)]);
    // Season 1 is marked watched; episode 3 of season 2 is untouched.
    f.store
        .mutate_playback(
            f.media_id,
            &store::playback::PlaybackMutation {
                user_id: user(),
                scope: domain::UnitScope::Season(1),
                played: Some(true),
                favorite: None,
                expected_generation: None,
            },
            5_000,
        )
        .unwrap();

    // No per-episode row exists for E02, yet the season assertion makes it read as played.
    assert!(
        f.store
            .unit_state(user(), f.media_id, 1, 2)
            .unwrap()
            .is_none(),
        "the mark writes the season scope, not fabricated episode rows"
    );
    let e02 = f
        .store
        .effective_unit_state(user(), f.media_id, UnitKey::new(1, 2))
        .unwrap()
        .expect("season assertion makes E02 visible");
    assert!(e02.played);
    assert_eq!(
        f.store
            .effective_played_assertion(user(), f.media_id, UnitKey::new(1, 2))
            .unwrap()
            .unwrap()
            .scope,
        PlayedAssertionScope::Season
    );

    let other_season = f
        .store
        .effective_unit_state(user(), f.media_id, UnitKey::new(2, 1))
        .unwrap();
    assert!(
        other_season.is_none_or(|state| !state.played),
        "another season is unaffected by the season-1 mark"
    );

    // Unmarking removes the assertion again.
    f.store
        .mutate_playback(
            f.media_id,
            &store::playback::PlaybackMutation {
                user_id: user(),
                scope: domain::UnitScope::Season(1),
                played: Some(false),
                favorite: None,
                expected_generation: None,
            },
            6_000,
        )
        .unwrap();
    let after = f
        .store
        .effective_unit_state(user(), f.media_id, UnitKey::new(1, 2))
        .unwrap();
    assert!(after.is_none_or(|state| !state.played));
}

#[test]
fn mark_does_not_advance_the_watch_clock() {
    let (_tmp, f) = seed(&[(1, 1)]);
    commit_progress(&f, &f.files[0], 1, 1, 120_000, 2_700_000);
    let before = f
        .store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        before.updated_at, 2,
        "progress at 2000 ms set the activity clock (seconds)"
    );

    f.store
        .mutate_playback(
            f.media_id,
            &store::playback::PlaybackMutation {
                user_id: user(),
                scope: domain::UnitScope::Episode {
                    season: 1,
                    episode: 1,
                },
                played: Some(true),
                favorite: None,
                expected_generation: None,
            },
            9_000,
        )
        .unwrap();
    let after = f
        .store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.updated_at, before.updated_at,
        "a mark must not look like watching activity"
    );
    assert!(after.played, "but the assertion itself lands");
}

#[test]
fn clear_history_removes_resume_and_assertions_keeps_favorite() {
    let (_tmp, f) = seed(&[(1, 1)]);
    commit_progress(&f, &f.files[0], 1, 1, 1_500_000, 2_700_000);
    f.store
        .mutate_playback(
            f.media_id,
            &store::playback::PlaybackMutation {
                user_id: user(),
                scope: domain::UnitScope::Episode {
                    season: 1,
                    episode: 1,
                },
                played: Some(true),
                favorite: Some(true),
                expected_generation: None,
            },
            3_000,
        )
        .unwrap();

    let cleared = f
        .store
        .clear_units(user(), Some(&[f.media_id]), None)
        .unwrap();
    assert_eq!(cleared, 1);

    let resume = f
        .store
        .resolve_playback(user(), &episode(&f, 1, 1), None, None)
        .unwrap();
    assert_eq!(
        resume.status,
        ResumeStatus::NoProgress,
        "clearing history drops the resume binding, so nothing resumes"
    );
    assert_eq!(resume.start_ms, 0);

    let assertion = f
        .store
        .effective_played_assertion(user(), f.media_id, UnitKey::new(1, 1))
        .unwrap();
    assert!(assertion.is_none(), "the played assertion is cleared too");

    let state = f
        .store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .unwrap();
    assert!(
        state.favorite,
        "the favourite is a library decision, not history"
    );
    assert!(!state.played);
    assert_eq!(state.position_ms, 0);
}

/// Activity columns are seconds while the played clock is milliseconds: a
/// heartbeat at 1700000010000 ms records activity at 1700000010 s.
#[test]
fn event_activity_clock_is_seconds_not_milliseconds() {
    let (tmp, f) = seed(&[(1, 1)]);
    let file = f.files[0].clone();
    let session_id = f
        .store
        .issue_play_session(
            user(),
            "device_web",
            file.file_id,
            f.media_id,
            UnitKey::new(1, 1),
            &file.version,
            1_000,
        )
        .unwrap();
    let generation = f
        .store
        .play_session_binding(user(), "device_web", &session_id)
        .unwrap()
        .unwrap()
        .generation;
    let event = store::playback::BoundPlaybackEvent {
        user_id: user(),
        device_id: "device_web".into(),
        session_id,
        generation,
        event_sequence: Some(1),
        item: episode(&f, 1, 1),
        source_id: PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        },
        source_version: file.version.source_version.clone(),
        position_ms: 2_600_000,
        duration_ms: Some(2_700_000),
        audio_track: None,
        subtitle_track: None,
        paused: false,
        rate_bps: 0,
        client: Some("Web".into()),
        device_name: None,
        client_version: None,
        play_method: None,
        event_type: PlaybackEventType::Progress,
    };
    f.store
        .commit_playback_event(&event, 1_700_000_010_000)
        .unwrap();
    let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let (activity, played_ms): (i64, Option<i64>) = conn
        .query_row(
            "SELECT watch_activity_at, played_changed_at_ms FROM playback_units
             WHERE season = 1 AND episode = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(activity, 1_700_000_010);
    assert_eq!(played_ms, Some(1_700_000_010_000));
}

fn mark(f: &Fixture, scope: domain::UnitScope, played: bool, now_ms: i64) {
    f.store
        .mutate_playback(
            f.media_id,
            &store::playback::PlaybackMutation {
                user_id: user(),
                scope,
                played: Some(played),
                favorite: None,
                expected_generation: None,
            },
            now_ms,
        )
        .unwrap();
}

#[test]
fn newer_season_assertion_overrides_older_episode_and_ties_go_to_episode() {
    let (_tmp, f) = seed(&[(1, 1), (1, 2)]);
    let e01 = UnitKey::new(1, 1);
    mark(
        &f,
        domain::UnitScope::Episode {
            season: 1,
            episode: 1,
        },
        true,
        1_000,
    );
    assert!(
        f.store
            .effective_played_assertion(user(), f.media_id, e01)
            .unwrap()
            .unwrap()
            .played
    );
    mark(&f, domain::UnitScope::Season(1), false, 2_000);
    let effective = f
        .store
        .effective_played_assertion(user(), f.media_id, e01)
        .unwrap();
    assert!(
        effective.as_ref().is_none_or(|a| !a.played),
        "season false@2000 must beat episode true@1000: {effective:?}"
    );

    // Same change time: the more specific episode assertion wins.
    let (_tmp2, g) = seed(&[(1, 1), (1, 2)]);
    mark(&g, domain::UnitScope::Season(1), true, 3_000);
    mark(
        &g,
        domain::UnitScope::Episode {
            season: 1,
            episode: 1,
        },
        false,
        3_000,
    );
    let tie = g
        .store
        .effective_played_assertion(user(), g.media_id, e01)
        .unwrap();
    assert!(
        tie.is_none_or(|a| !a.played),
        "tie goes to the episode scope"
    );
}

fn favorite(f: &Fixture, scope: domain::UnitScope, value: bool, now_ms: i64) {
    f.store
        .mutate_playback(
            f.media_id,
            &store::playback::PlaybackMutation {
                user_id: user(),
                scope,
                played: None,
                favorite: Some(value),
                expected_generation: None,
            },
            now_ms,
        )
        .unwrap();
}

const E11: domain::UnitScope = domain::UnitScope::Episode {
    season: 1,
    episode: 1,
};

#[test]
fn favorite_only_row_reads_favorite_with_second_clock() {
    let (_tmp, f) = seed(&[(1, 1)]);
    favorite(&f, E11, true, 1_700_000_000_000);
    let state = f
        .store
        .effective_unit_state(user(), f.media_id, UnitKey::new(1, 1))
        .unwrap()
        .expect("a favorite-only row is a fact");
    assert!(state.favorite);
    assert!(!state.played);
    assert_eq!(state.favorite_at, 1_700_000_000, "favorite_at is seconds");
    assert_eq!(state.updated_at, 0, "a favorite is not viewing activity");

    favorite(&f, domain::UnitScope::Whole, true, 1_700_000_005_000);
    favorite(&f, domain::UnitScope::Whole, false, 1_700_000_006_000);
    let unfavorited = f
        .store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .unwrap();
    assert!(!unfavorited.favorite, "whole unfavorite clears the media");
    assert_eq!(unfavorited.favorite_at, 0);
}

#[test]
fn scoped_marks_resolve_by_latest_and_keep_resume_package() {
    let (_tmp, f) = seed(&[(1, 1), (1, 2), (2, 1)]);
    commit_progress(&f, &f.files[0], 1, 1, 120_000, 2_700_000);
    let resume_before = f
        .store
        .resolve_playback(user(), &episode(&f, 1, 1), None, None)
        .unwrap();
    let activity_before = f
        .store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .unwrap();

    mark(&f, domain::UnitScope::Season(1), true, 1_700_000_000_000);
    mark(&f, E11, false, 1_700_000_001_000);
    let episode_after_later_false = f
        .store
        .effective_unit_state(user(), f.media_id, UnitKey::new(1, 1))
        .unwrap()
        .unwrap();
    assert!(!episode_after_later_false.played);
    let resume_package_after_mark = f
        .store
        .resolve_playback(user(), &episode(&f, 1, 1), None, None)
        .unwrap();
    assert_eq!(resume_package_after_mark.start_ms, resume_before.start_ms);
    assert_eq!(resume_package_after_mark.status, ResumeStatus::Compatible);

    mark(&f, domain::UnitScope::Season(1), true, 1_700_000_002_000);
    let episode_after_latest_season_true = f
        .store
        .effective_unit_state(user(), f.media_id, UnitKey::new(1, 1))
        .unwrap()
        .unwrap();
    assert!(episode_after_latest_season_true.played);
    let resolved = f
        .store
        .resolve_playback(user(), &episode(&f, 1, 1), None, None)
        .unwrap();
    assert_eq!(
        resolved.status,
        ResumeStatus::AlreadyPlayed,
        "the effective season assertion decides AlreadyPlayed"
    );
    let activity_after_mark = f
        .store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .unwrap();
    assert_eq!(activity_after_mark.updated_at, activity_before.updated_at);
    assert_eq!(activity_after_mark.position_ms, activity_before.position_ms);
    assert_eq!(activity_after_mark.duration_ms, activity_before.duration_ms);
    assert!(
        f.store
            .effective_unit_state(user(), f.media_id, UnitKey::new(2, 1))
            .unwrap()
            .is_none_or(|unit| !unit.played),
        "an unrelated season is untouched"
    );
}
