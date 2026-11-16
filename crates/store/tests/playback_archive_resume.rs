//! Repeated live intervals of one issued session merge into one play log.
mod playback_event_support;
use playback_event_support::*;

fn sample(f: &Fixture, session: (&str, i64), sequence: i64, position: i64, now: i64) {
    let mut bound = event(
        f,
        episode(f, 1),
        session,
        &f.file,
        position,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(sequence);
    bound.duration_ms = Some(3_000_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, now).unwrap(),
        EventOutcome::Applied
    );
}

#[test]
fn resumed_session_merges_each_archive_path_without_counting_sleep_as_watch_time() {
    for ending in ["close", "stop", "superseded"] {
        let (_tmp, f) = seed();
        let user = USER.parse().unwrap();
        let item = episode(&f, 1);
        let session = issue(&f, &item, &f.file);
        let session_ref = (&*session.0, session.1);
        sample(&f, session_ref, 1, 6_000, 2_000);
        sample(&f, session_ref, 2, 7_000, 3_000);
        assert_eq!(f.store.sweep_stale_sessions(100, 30).unwrap(), 1);
        let initial = f
            .store
            .list_logs(10, None, None, None, 100, Some(user))
            .unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(
            (
                initial[0].started_at,
                initial[0].start_position_ms,
                initial[0].watched_ms
            ),
            (2, 6_000, 1_000)
        );
        sample(&f, session_ref, 3, 8_000, 104_000);
        sample(&f, session_ref, 4, 9_000, 105_000);
        let (end_position, watched_ms) = match ending {
            "close" => {
                let log = f
                    .store
                    .close_session(user, "device_web", 106)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    log.id, initial[0].id,
                    "close returns the merged persisted log"
                );
                assert_eq!(log.watched_ms, 2_000);
                (9_000, 2_000)
            }
            "stop" => {
                let mut stop = event(
                    &f,
                    item,
                    session_ref,
                    &f.file,
                    10_000,
                    PlaybackEventType::Stopped,
                );
                stop.event_sequence = Some(5);
                stop.duration_ms = Some(3_000_000);
                assert_eq!(
                    f.store.commit_playback_event(&stop, 106_000).unwrap(),
                    EventOutcome::Applied
                );
                assert_eq!(
                    f.store.commit_playback_event(&stop, 107_000).unwrap(),
                    EventOutcome::IgnoredStale
                );
                (10_000, 3_000)
            }
            "superseded" => {
                let next = issue(&f, &item, &f.file);
                sample(&f, (&next.0, next.1), 1, 10_000, 106_000);
                (9_000, 2_000)
            }
            _ => unreachable!(),
        };
        let logs = f
            .store
            .list_logs(10, None, None, None, 108, Some(user))
            .unwrap();
        assert_eq!(
            logs.len(),
            1,
            "one logical session retains one log after {ending}"
        );
        assert_eq!(logs[0].id, initial[0].id);
        assert_eq!((logs[0].started_at, logs[0].start_position_ms), (2, 6_000));
        assert_eq!(
            (
                logs[0].ended_at,
                logs[0].end_position_ms,
                logs[0].watched_ms
            ),
            (106, end_position, watched_ms)
        );
        assert_eq!(logs[0].duration_ms, Some(3_000_000));
    }
}

#[test]
fn stop_after_sweep_updates_existing_log_without_counting_sleep() {
    for position in [Some(9_000), None] {
        let (_tmp, f) = seed();
        let user = USER.parse().unwrap();
        let item = episode(&f, 1);
        let session = issue(&f, &item, &f.file);
        let session_ref = (&*session.0, session.1);
        sample(&f, session_ref, 1, 6_000, 2_000);
        sample(&f, session_ref, 2, 7_000, 3_000);
        assert_eq!(f.store.sweep_stale_sessions(100, 30).unwrap(), 1);
        let mut stop = event(
            &f,
            item,
            session_ref,
            &f.file,
            9_000,
            PlaybackEventType::Stopped,
        );
        stop.event_sequence = Some(3);
        stop.position_ms = position;
        stop.duration_ms = Some(3_000_000);
        assert_eq!(
            f.store.commit_playback_event(&stop, 106_000).unwrap(),
            EventOutcome::Applied
        );
        let logs = f
            .store
            .list_logs(10, None, None, None, 108, Some(user))
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!((logs[0].started_at, logs[0].start_position_ms), (2, 6_000));
        assert_eq!(
            (logs[0].ended_at, logs[0].end_position_ms),
            (106, position.unwrap_or(7_000))
        );
        assert_eq!(logs[0].watched_ms, 1_000);
        assert!(f.store.get_session(user, "device_web").unwrap().is_none());
        let binding = f
            .store
            .play_session_binding(user, "device_web", &session.0)
            .unwrap()
            .unwrap();
        assert_eq!(binding.state, PlaySessionIdState::Closed);
        assert_eq!(binding.last_event_sequence, Some(3));
    }
}
