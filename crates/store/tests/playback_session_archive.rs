//! Session sequence ordering survives live-row archival without preventing resumed playback.
mod playback_event_support;
use playback_event_support::*;

fn archive(f: &Fixture, sweep: bool, now: i64) {
    if sweep {
        assert_eq!(f.store.sweep_stale_sessions(now, 30).unwrap(), 1);
        assert_eq!(f.store.sweep_stale_sessions(now + 1, 30).unwrap(), 0);
    } else {
        assert!(
            f.store
                .close_session(USER.parse().unwrap(), "device_web", now)
                .unwrap()
                .is_some()
        );
        assert!(
            f.store
                .close_session(USER.parse().unwrap(), "device_web", now + 1)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        f.store
            .get_session(USER.parse().unwrap(), "device_web")
            .unwrap()
            .is_none()
    );
}

fn refuse_old_events(f: &Fixture, bound: &mut BoundPlaybackEvent) {
    for event_type in [
        PlaybackEventType::Playing,
        PlaybackEventType::Progress,
        PlaybackEventType::Stopped,
    ] {
        for sequence in [2, 3] {
            bound.event_type = event_type;
            bound.event_sequence = Some(sequence);
            bound.position_ms = Some(2_300_000);
            assert_eq!(
                f.store.commit_playback_event(bound, 102_000).unwrap(),
                EventOutcome::IgnoredStale
            );
        }
    }
}

fn archived_session_retains_sequence(sweep: bool) {
    let (_tmp, f) = seed();
    let user = USER.parse().unwrap();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item.clone(),
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(3);
    assert_eq!(
        f.store.commit_playback_event(&bound, 2_000).unwrap(),
        EventOutcome::Applied
    );
    let next = issue(&f, &item, &f.file);
    archive(&f, sweep, 100);
    assert_eq!(
        f.store
            .play_session_binding(user, "device_web", &session.0)
            .unwrap()
            .unwrap()
            .last_event_sequence,
        Some(3)
    );
    let before_unit = f.store.unit_state(user, f.media_id, 1, 1).unwrap();
    refuse_old_events(&f, &mut bound);
    assert_eq!(
        f.store.unit_state(user, f.media_id, 1, 1).unwrap(),
        before_unit
    );
    assert!(f.store.get_session(user, "device_web").unwrap().is_none());
    let logs = f
        .store
        .list_logs(10, None, None, None, 103, Some(user))
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].end_position_ms, 6_000);
    bound.event_type = PlaybackEventType::Progress;
    bound.event_sequence = None;
    bound.position_ms = Some(7_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 104_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(7_000));
    archive(&f, sweep, 200);
    bound.event_sequence = Some(3);
    assert_eq!(
        f.store.commit_playback_event(&bound, 204_000).unwrap(),
        EventOutcome::IgnoredStale,
        "an unsequenced resume must not erase the persisted sequence watermark"
    );
    bound.event_sequence = Some(4);
    bound.position_ms = Some(8_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 205_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(8_000));
    let binding = f
        .store
        .play_session_binding(user, "device_web", &session.0)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Active);
    assert_eq!(binding.last_event_sequence, Some(4));
    let next_binding = f
        .store
        .play_session_binding(user, "device_web", &next.0)
        .unwrap()
        .unwrap();
    assert_eq!(next_binding.state, PlaySessionIdState::Pending);
    let mut fresh = event(
        &f,
        item,
        (&next.0, next.1),
        &f.file,
        9_000,
        PlaybackEventType::Playing,
    );
    fresh.event_sequence = Some(1);
    assert_eq!(
        f.store.commit_playback_event(&fresh, 206_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(9_000));
}

#[test]
fn closed_live_session_rejects_old_sequences_and_resumes_new_events() {
    archived_session_retains_sequence(false);
}

#[test]
fn swept_live_session_rejects_old_sequences_and_resumes_new_events() {
    archived_session_retains_sequence(true);
}

#[test]
fn failed_archive_keeps_the_binding_live_with_its_session() {
    let (tmp, f) = seed();
    let user = USER.parse().unwrap();
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
    let before = f.store.get_session(user, "device_web").unwrap();
    // Inject an archive failure; all observations below go through the Store API.
    let diagnostic = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    diagnostic
        .execute_batch(
            "CREATE TRIGGER fail_playback_archive BEFORE INSERT ON playback_logs
         BEGIN SELECT RAISE(FAIL, 'archive fault'); END;",
        )
        .unwrap();
    assert!(f.store.close_session(user, "device_web", 100).is_err());
    assert_eq!(f.store.get_session(user, "device_web").unwrap(), before);
    let binding = f
        .store
        .play_session_binding(user, "device_web", &session.0)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Active);
    assert_eq!(binding.last_event_sequence, Some(3));
    assert!(
        f.store
            .list_logs(10, None, None, None, 101, Some(user))
            .unwrap()
            .is_empty()
    );
    bound.event_sequence = Some(4);
    bound.position_ms = Some(7_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 102_000).unwrap(),
        EventOutcome::Applied
    );
}

#[test]
fn failed_event_transaction_does_not_advance_binding_sequence() {
    let (tmp, f) = seed();
    let user = USER.parse().unwrap();
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
    let before = f.store.get_session(user, "device_web").unwrap();
    let diagnostic = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    diagnostic
        .execute_batch(
            "CREATE TRIGGER fail_playback_unit BEFORE UPDATE ON playback_units
         BEGIN SELECT RAISE(FAIL, 'unit fault'); END;",
        )
        .unwrap();
    bound.event_sequence = Some(4);
    bound.position_ms = Some(7_000);
    assert!(f.store.commit_playback_event(&bound, 3_000).is_err());
    assert_eq!(f.store.get_session(user, "device_web").unwrap(), before);
    assert_eq!(
        f.store
            .play_session_binding(user, "device_web", &session.0)
            .unwrap()
            .unwrap()
            .last_event_sequence,
        Some(3)
    );
    diagnostic
        .execute_batch("DROP TRIGGER fail_playback_unit;")
        .unwrap();
    assert_eq!(
        f.store.commit_playback_event(&bound, 4_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(7_000));
}
