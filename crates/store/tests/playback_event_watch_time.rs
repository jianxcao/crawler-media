//! Watch-time accounting: a `Progress` event may claim elapsed wall time only while
//! playback is actually running. `paused` and `Stopped` are facts the event already
//! carries; these tests pin them at the public `commit_playback_event` seam.

mod playback_event_support;
use playback_event_support::*;

/// The live session row for `device`, which must still exist while playback is open.
fn live_row(f: &Fixture, device: &str) -> store::playback::SessionRow {
    let user: UserId = USER.parse().unwrap();
    f.store
        .get_session(user, device)
        .unwrap()
        .expect("the live session row")
}

fn watched_ms(f: &Fixture, device: &str) -> i64 {
    live_row(f, device).watched_ms
}

/// Commit a `Progress` heartbeat carrying `paused` for E01 of the merged file.
fn heartbeat(
    f: &Fixture,
    device: &str,
    session: (&str, i64),
    position_ms: i64,
    paused: bool,
    now_ms: i64,
) -> EventOutcome {
    let mut bound = event_on(
        f,
        device,
        episode(f, 1),
        session,
        &f.file,
        position_ms,
        PlaybackEventType::Progress,
    );
    bound.paused = paused;
    f.store
        .commit_playback_event(&bound, now_ms)
        .expect("commit heartbeat")
}

#[test]
fn paused_heartbeat_without_movement_claims_no_watch_time() {
    let (_tmp, f) = seed();
    let s = issue(&f, &episode(&f, 1), &f.file);

    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 5_000, false, 2_000),
        EventOutcome::Applied
    );
    assert_eq!(
        watched_ms(&f, "device_web"),
        0,
        "the first heartbeat has no earlier interval"
    );

    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 5_000, true, 3_000),
        EventOutcome::Applied
    );
    let row = live_row(&f, "device_web");
    assert!(row.paused, "the pause is recorded on the live row");
    assert_eq!(
        (row.position_ms, row.watched_ms),
        (5_000, 0),
        "a pause that moved nowhere claims no watch time"
    );
}

#[test]
fn paused_heartbeat_with_real_movement_claims_no_watch_time() {
    let (_tmp, f) = seed();
    let s = issue(&f, &episode(&f, 1), &f.file);

    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 5_000, false, 2_000),
        EventOutcome::Applied
    );
    // One second of wall time, one second of position: real movement, but the client reports the
    // pause happening at the end of that interval.
    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 6_000, true, 3_000),
        EventOutcome::Applied
    );
    let row = live_row(&f, "device_web");
    assert_eq!(
        row.position_ms, 6_000,
        "the pause still records where playback stopped"
    );
    assert!(row.paused, "the pause is recorded on the live row");
    assert_eq!(
        row.watched_ms, 0,
        "a paused heartbeat claims no watch time, even when the position advanced"
    );
}

#[test]
fn playing_heartbeat_within_the_seek_budget_claims_watch_time() {
    let (_tmp, f) = seed();
    let s = issue(&f, &episode(&f, 1), &f.file);

    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 5_000, false, 2_000),
        EventOutcome::Applied
    );
    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 6_000, false, 3_000),
        EventOutcome::Applied
    );
    let row = live_row(&f, "device_web");
    assert_eq!(
        (row.position_ms, row.watched_ms),
        (6_000, 1_000),
        "a running second-long interval is watched time"
    );

    // 预算边界:允许的时间 = 已过墙钟 + 2_000。1 s 的间隔恰好放行 3_000 ms 的位移。
    for (device, position_ms, watched) in [
        ("device_budget_in", 8_000, 1_000),
        ("device_budget_out", 8_001, 0),
    ] {
        let s = issue_on(&f, device, &episode(&f, 1), &f.file);
        assert_eq!(
            heartbeat(&f, device, (&s.0, s.1), 5_000, false, 2_000),
            EventOutcome::Applied
        );
        assert_eq!(
            heartbeat(&f, device, (&s.0, s.1), position_ms, false, 3_000),
            EventOutcome::Applied
        );
        assert_eq!(
            watched_ms(&f, device),
            watched,
            "1 s allows exactly 3_000 ms of movement"
        );
    }
}

#[test]
fn seek_disguised_as_progress_claims_no_watch_time_in_either_state() {
    let (_tmp, f) = seed();
    let item = episode(&f, 1);

    let playing = issue_on(&f, "device_seek_playing", &item, &f.file);
    assert_eq!(
        heartbeat(
            &f,
            "device_seek_playing",
            (&playing.0, playing.1),
            5_000,
            false,
            2_000
        ),
        EventOutcome::Applied
    );
    assert_eq!(
        heartbeat(
            &f,
            "device_seek_playing",
            (&playing.0, playing.1),
            105_000,
            false,
            3_000
        ),
        EventOutcome::Applied
    );
    assert_eq!(
        watched_ms(&f, "device_seek_playing"),
        0,
        "a seek is not watching"
    );

    let paused = issue_on(&f, "device_seek_paused", &item, &f.file);
    assert_eq!(
        heartbeat(
            &f,
            "device_seek_paused",
            (&paused.0, paused.1),
            5_000,
            false,
            2_000
        ),
        EventOutcome::Applied
    );
    assert_eq!(
        heartbeat(
            &f,
            "device_seek_paused",
            (&paused.0, paused.1),
            105_000,
            true,
            3_000
        ),
        EventOutcome::Applied
    );
    assert_eq!(
        watched_ms(&f, "device_seek_paused"),
        0,
        "a paused seek claims no watch time either"
    );
}

#[test]
fn paused_stop_still_records_its_final_position_and_watch_time() {
    let (_tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();
    let item = episode(&f, 1);
    let s = issue(&f, &item, &f.file);

    assert_eq!(
        heartbeat(&f, "device_web", (&s.0, s.1), 5_000, false, 2_000),
        EventOutcome::Applied
    );
    let mut stop = event(
        &f,
        item,
        (&s.0, s.1),
        &f.file,
        6_000,
        PlaybackEventType::Stopped,
    );
    stop.paused = true;
    assert_eq!(
        f.store.commit_playback_event(&stop, 3_000).unwrap(),
        EventOutcome::Applied
    );
    assert!(
        f.store.get_session(user, "device_web").unwrap().is_none(),
        "a stop archives the live row"
    );
    let archived = f
        .store
        .play_session_binding(user, "device_web", &s.0)
        .unwrap()
        .unwrap();
    assert_eq!(
        archived.state,
        PlaySessionIdState::Closed,
        "a stop closes the id row"
    );

    let logs = f
        .store
        .list_logs(10, None, None, None, 3_000, Some(user))
        .unwrap();
    assert_eq!(logs.len(), 1, "the stop writes exactly one play log");
    let log = logs.first().expect("the stop writes one play log");
    assert_eq!(
        log.end_position_ms, 6_000,
        "the stop's final position is archived"
    );
    assert_eq!(
        log.watched_ms, 1_000,
        "a pause on the stop must not suppress the interval the stop itself claims"
    );
}

#[test]
fn new_session_on_same_device_starts_fresh_and_logs_the_old_once() {
    let (_tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();
    let a = issue(&f, &episode(&f, 1), &f.file);
    assert_eq!(
        heartbeat(&f, "device_web", (&a.0, a.1), 0, false, 2_000),
        EventOutcome::Applied
    );
    assert_eq!(
        heartbeat(&f, "device_web", (&a.0, a.1), 10_000, false, 12_000),
        EventOutcome::Applied
    );
    assert_eq!(
        watched_ms(&f, "device_web"),
        10_000,
        "positive control: A watched 10s"
    );

    let b = issue(&f, &episode(&f, 1), &f.file);
    assert_eq!(
        heartbeat(&f, "device_web", (&b.0, b.1), 10_000, false, 13_000),
        EventOutcome::Applied
    );
    let row = live_row(&f, "device_web");
    assert_eq!(row.play_session_id.as_deref(), Some(b.0.as_str()));
    assert_eq!(row.watched_ms, 0, "B must not inherit A's watch time");
    assert_eq!(row.started_at, 13, "B starts at its own first event");

    let logs = f
        .store
        .playback_log_page(
            &store::playback::PlaybackLogScope {
                before: None,
                before_id: None,
                since: None,
                user_id: Some(user),
                visible_media: None,
            },
            100,
        )
        .unwrap()
        .entries;
    assert_eq!(logs.len(), 1, "A is archived exactly once: {logs:?}");
    assert_eq!(logs[0].watched_ms, 10_000);
}
