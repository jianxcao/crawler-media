//! C06: an event that omits its duration keeps the resume duration only while
//! it stays on the same bound physical source.

#[path = "playback_event_support/mod.rs"]
mod playback_event_support;
use playback_event_support::*;

fn user() -> UserId {
    USER.parse().unwrap()
}

#[allow(clippy::too_many_arguments)]
fn commit(
    f: &Fixture,
    file: &LibraryFile,
    session: (&str, i64),
    seq: i64,
    position: i64,
    duration: Option<i64>,
    kind: PlaybackEventType,
    now_ms: i64,
) -> EventOutcome {
    let mut bound = event(f, episode(f, 1), session, file, position, kind);
    bound.duration_ms = duration;
    bound.event_sequence = Some(seq);
    f.store
        .commit_playback_event(&bound, now_ms)
        .expect("commit event")
}

fn unit(f: &Fixture) -> store::UnitState {
    f.store
        .unit_state(user(), f.media_id, 1, 1)
        .unwrap()
        .expect("unit row")
}

#[test]
fn stop_without_duration_keeps_same_source_completion() {
    let (_tmp, f) = seed();
    let (session, generation) = issue(&f, &episode(&f, 1), &f.file);
    let s = (session.as_str(), generation);
    commit(
        &f,
        &f.file,
        s,
        1,
        50_000,
        Some(100_000),
        PlaybackEventType::Progress,
        2_000,
    );
    commit(
        &f,
        &f.file,
        s,
        2,
        95_000,
        None,
        PlaybackEventType::Stopped,
        3_000,
    );
    let same_source_after_stop = unit(&f);
    assert_eq!(same_source_after_stop.duration_ms, Some(100_000));
    assert!(same_source_after_stop.played);
    let play_count_after_first_stop = same_source_after_stop.play_count;
    assert_eq!(play_count_after_first_stop, 1);

    commit(
        &f,
        &f.file,
        s,
        3,
        95_000,
        None,
        PlaybackEventType::Stopped,
        4_000,
    );
    let play_count_after_duplicate_stop = unit(&f).play_count;
    assert_eq!(play_count_after_duplicate_stop, play_count_after_first_stop);
}

#[test]
fn new_source_without_duration_does_not_inherit_old_duration() {
    let (_tmp, f) = seed();
    let (session, generation) = issue(&f, &episode(&f, 1), &f.file);
    commit(
        &f,
        &f.file,
        (session.as_str(), generation),
        1,
        50_000,
        Some(100_000),
        PlaybackEventType::Progress,
        2_000,
    );

    let mut replacement = f.file.clone();
    replacement.file_id = "00000000-0000-0000-0000-000000000777".parse().unwrap();
    replacement.path = "Merged Show/S01E01-E02 v2.mkv".into();
    replacement.duration_ms = None;
    let scope = WriteScope {
        library_id: f.library_id.parse().unwrap(),
        config_revision: 0,
    };
    f.store
        .publish_files(
            &scope,
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
    let (session2, generation2) = issue_on(&f, "device_tv", &episode(&f, 1), &replacement);
    let mut bound = event_on(
        &f,
        "device_tv",
        episode(&f, 1),
        (session2.as_str(), generation2),
        &replacement,
        95_000,
        PlaybackEventType::Progress,
    );
    bound.duration_ms = None;
    assert_eq!(
        f.store.commit_playback_event(&bound, 4_000).unwrap(),
        EventOutcome::Applied
    );
    let new_source_package = unit(&f);
    assert_eq!(new_source_package.duration_ms, None);
    assert!(
        !new_source_package.played,
        "unknown duration never auto-completes"
    );
}

#[test]
fn duration_first_reported_on_stop_is_persisted_in_archived_log_and_unit_state() {
    let (_tmp, f) = seed();
    let (session, generation) = issue(&f, &episode(&f, 1), &f.file);
    let s = (session.as_str(), generation);

    // Progress arrives before duration is known.
    assert_eq!(
        commit(
            &f,
            &f.file,
            s,
            1,
            30_000,
            None,
            PlaybackEventType::Progress,
            2_000,
        ),
        EventOutcome::Applied
    );
    // Stop is the first event that reports duration (100s) at 95s (>= 90% completed).
    assert_eq!(
        commit(
            &f,
            &f.file,
            s,
            2,
            95_000,
            Some(100_000),
            PlaybackEventType::Stopped,
            3_000,
        ),
        EventOutcome::Applied
    );

    let after = unit(&f);
    assert_eq!(after.duration_ms, Some(100_000));
    assert!(after.played);
    assert_eq!(after.play_count, 1);

    let logs = f
        .store
        .list_logs(20, None, None, None, 10_000, Some(user()))
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].duration_ms, Some(100_000));
    assert!(
        logs[0].completed,
        "archived play log must agree with unit played state when Stop supplies duration"
    );
    assert_eq!(logs[0].end_position_ms, 95_000);

    // Replaying Stop must not create a second play log or double-count plays.
    assert_eq!(
        commit(
            &f,
            &f.file,
            s,
            3,
            95_000,
            Some(100_000),
            PlaybackEventType::Stopped,
            4_000,
        ),
        EventOutcome::IgnoredStale
    );
    assert_eq!(
        f.store
            .list_logs(20, None, None, None, 10_000, Some(user()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(unit(&f).play_count, 1);
}
