//! Sequence acceptance is enforced by the Store transaction, after earlier events commit.
mod playback_event_support;
use playback_event_support::*;

#[test]
fn older_and_duplicate_sequences_cannot_change_unit_or_session_facts() {
    for event_type in [PlaybackEventType::Progress, PlaybackEventType::Stopped] {
        for sequence in [2, 3] {
            let (_tmp, f) = seed();
            let user = USER.parse().unwrap();
            let item = episode(&f, 1);
            let session = issue(&f, &item, &f.file);
            let mut latest = event(
                &f,
                item.clone(),
                (&session.0, session.1),
                &f.file,
                6_000,
                PlaybackEventType::Progress,
            );
            latest.event_sequence = Some(3);
            assert_eq!(
                f.store.commit_playback_event(&latest, 2_000).unwrap(),
                EventOutcome::Applied
            );
            let before_unit = f.store.unit_state(user, f.media_id, 1, 1).unwrap();
            let before_session = f.store.get_session(user, "device_web").unwrap();
            let mut late = latest.clone();
            late.event_sequence = Some(sequence);
            late.event_type = event_type;
            late.position_ms = Some(2_300_000);
            late.audio_track = Some("embedded:9".into());
            assert_eq!(
                f.store.commit_playback_event(&late, 3_000).unwrap(),
                EventOutcome::IgnoredStale,
                "sequence {sequence} after sequence 3 must be ignored for {event_type:?}",
            );
            assert_eq!(
                f.store.unit_state(user, f.media_id, 1, 1).unwrap(),
                before_unit
            );
            assert_eq!(
                f.store.get_session(user, "device_web").unwrap(),
                before_session
            );
            assert!(
                f.store
                    .list_logs(10, None, None, None, 3, Some(user))
                    .unwrap()
                    .is_empty()
            );
            let binding = f
                .store
                .play_session_binding(user, "device_web", &session.0)
                .unwrap()
                .unwrap();
            assert_eq!(binding.state, PlaySessionIdState::Active);
            latest.event_sequence = Some(4);
            latest.position_ms = Some(7_000);
            assert_eq!(
                f.store.commit_playback_event(&latest, 4_000).unwrap(),
                EventOutcome::Applied
            );
            assert_eq!(unit_position(&f, 1), Some(7_000));
        }
    }
}

#[test]
fn missing_position_does_not_bypass_sequence_guard() {
    let (_tmp, f) = seed();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item,
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(3);
    f.store.commit_playback_event(&bound, 2_000).unwrap();
    bound.event_sequence = Some(2);
    bound.position_ms = None;
    bound.event_type = PlaybackEventType::Stopped;
    assert_eq!(
        f.store.commit_playback_event(&bound, 3_000).unwrap(),
        EventOutcome::IgnoredStale
    );
    assert!(
        f.store
            .get_session(USER.parse().unwrap(), "device_web")
            .unwrap()
            .is_some()
    );
}

#[test]
fn unsequenced_events_keep_jellyfin_progress_and_stop_behavior() {
    let (_tmp, f) = seed();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item,
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(3);
    f.store.commit_playback_event(&bound, 2_000).unwrap();
    bound.event_sequence = None;
    bound.position_ms = Some(7_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 3_000).unwrap(),
        EventOutcome::Applied
    );
    bound.event_type = PlaybackEventType::Stopped;
    bound.position_ms = Some(8_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 4_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(8_000));
    assert!(
        f.store
            .get_session(USER.parse().unwrap(), "device_web")
            .unwrap()
            .is_none()
    );
}

#[test]
fn new_session_sequence_is_independent_of_superseded_session() {
    let (_tmp, f) = seed();
    let item = episode(&f, 1);
    for (sequence, position) in [(100, 6_000), (1, 7_000)] {
        let session = issue(&f, &item, &f.file);
        let mut bound = event(
            &f,
            item.clone(),
            (&session.0, session.1),
            &f.file,
            position,
            PlaybackEventType::Progress,
        );
        bound.event_sequence = Some(sequence);
        assert_eq!(
            f.store.commit_playback_event(&bound, position).unwrap(),
            EventOutcome::Applied
        );
    }
    let row = f
        .store
        .get_session(USER.parse().unwrap(), "device_web")
        .unwrap()
        .unwrap();
    assert_eq!(row.last_event_sequence, Some(1));
    assert_eq!(row.position_ms, 7_000);
}

#[test]
fn rejected_event_cannot_advance_the_issued_sequence_watermark() {
    let (_tmp, f) = seed();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item,
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(1);
    assert_eq!(
        f.store.commit_playback_event(&bound, 2_000).unwrap(),
        EventOutcome::Applied
    );
    let mut invalid = bound.clone();
    invalid.event_sequence = Some(100);
    invalid.source_version = "invalid".into();
    assert_eq!(
        f.store.commit_playback_event(&invalid, 3_000).unwrap(),
        EventOutcome::IgnoredStale
    );
    bound.event_sequence = Some(2);
    bound.position_ms = Some(7_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 4_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(7_000));
}
