//! Session-bound playback events: an event may only write the target and physical source
//! that were *issued* to its session, and only while that source generation still holds.
//!
//! These are the review-focus regressions: one physical file serving E01 and E02 must keep
//! the two units independent, a late event for the other episode must not close or move the
//! active session, a replaced source must not accept writes from the old session, and a
//! repeated `Stop` must be a no-op rather than a second close.

mod playback_event_support;
use playback_event_support::*;

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
            &event(
                &f,
                e01.clone(),
                (&s01.0, s01.1),
                &f.file,
                2_300_000,
                PlaybackEventType::Stopped,
            ),
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
            &event(
                &f,
                e02.clone(),
                (&s02.0, s02.1),
                &f.file,
                5_000,
                PlaybackEventType::Progress,
            ),
            3_000,
        )
        .unwrap();
    assert_eq!(out, EventOutcome::Applied);

    let e02_after = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert_eq!(e02_after.start_ms, 5_000, "the E02 heartbeat moves E02");
    assert_eq!(
        e02_after.status,
        ResumeStatus::Compatible,
        "the E02 binding matches its source"
    );
    let e01_after = f.store.resolve_playback(user, &e01, None, None).unwrap();
    assert_eq!(
        e01_after, e01_before,
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
            &event(
                &f,
                e02.clone(),
                (&s02.0, s02.1),
                &f.file,
                5_000,
                PlaybackEventType::Progress,
            ),
            3_000,
        )
        .unwrap();
    let e02_before = f.store.resolve_playback(user, &e02, None, None).unwrap();
    // Snapshots for the "nothing moves" assertions are taken *before* the refused event: a
    // snapshot read after it would compare a state with itself and could never fail.
    let unit_before = f.store.unit_state(user, f.media_id, 1, 2).unwrap();
    assert_eq!(
        unit_before
            .as_ref()
            .map(|u| (u.position_ms, u.played, u.favorite, u.play_count)),
        Some((5_000, false, false, 0)),
        "positive control: the E02 heartbeat wrote exactly this unit fact"
    );

    let e01_before = f.store.resolve_playback(user, &e01, None, None).unwrap();
    assert_eq!(
        e01_before.status,
        ResumeStatus::NoProgress,
        "E01 was never played"
    );

    // A late E01 Stop arrives carrying E01's target but E02's session: refused, nothing moves.
    let stale = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e01.clone(),
                (&s02.0, s02.1),
                &f.file,
                2_300_000,
                PlaybackEventType::Stopped,
            ),
            4_000,
        )
        .unwrap();
    assert_eq!(
        stale,
        EventOutcome::IgnoredStale,
        "a late event for another episode is stale"
    );

    let e02_after = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert_eq!(
        e02_after, e02_before,
        "the late E01 event must not move any field of E02's resume record"
    );
    let unit_after = f.store.unit_state(user, f.media_id, 1, 2).unwrap();
    assert_eq!(
        unit_after, unit_before,
        "the late E01 event must not write any playback_units fact (position/played/play_count/...)"
    );
    let e01_after = f.store.resolve_playback(user, &e01, None, None).unwrap();
    assert_eq!(
        e01_after, e01_before,
        "the late E01 event must not write any field of E01's resume record"
    );

    // E02's own session is still active: its next heartbeat still applies.
    let nex = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&s02.0, s02.1),
                &f.file,
                9_000,
                PlaybackEventType::Progress,
            ),
            5_000,
        )
        .unwrap();
    assert_eq!(
        nex,
        EventOutcome::Applied,
        "the active session was never closed"
    );

    // E01's own session still works for E01.
    let e01_ok = f
        .store
        .commit_playback_event(
            &event_on(
                &f,
                "device_tv",
                e01.clone(),
                (&s01.0, s01.1),
                &f.file,
                7_000,
                PlaybackEventType::Progress,
            ),
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
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                5_000,
                PlaybackEventType::Progress,
            ),
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
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &replacement,
                900_000,
                PlaybackEventType::Progress,
            ),
            4_000,
        )
        .unwrap();
    assert_eq!(
        stale,
        EventOutcome::IgnoredStale,
        "issued session cannot write the replacement"
    );

    let mut stale_old = event(
        &f,
        e02.clone(),
        (&session.0, session.1),
        &f.file,
        900_000,
        PlaybackEventType::Progress,
    );
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
        resume.status,
        ResumeStatus::Compatible,
        "the bound source is the issued one"
    );
    assert_eq!(
        resume.target.file.file_id, f.file.file_id,
        "no implicit switch to the replacement source"
    );
    drop(tmp);
}

#[test]
fn repeated_stop_is_a_single_close() {
    let (_tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();
    let e02 = episode(&f, 2);
    let session = issue(&f, &e02, &f.file);

    let first = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                2_300_000,
                PlaybackEventType::Stopped,
            ),
            2_000,
        )
        .unwrap();
    assert_eq!(first, EventOutcome::Applied);
    // Positive control for every "the ignored events changed nothing" assertion below: the one
    // Stop must have written this exact state, or the comparison would pass on an empty row.
    let unit_after_first = f.store.unit_state(user, f.media_id, 1, 2).unwrap();
    assert_eq!(
        unit_after_first
            .as_ref()
            .map(|u| (u.position_ms, u.played, u.play_count)),
        Some((2_300_000, true, 1)),
        "the single Stop writes the position, the played flag and one play"
    );
    let resume_after_first = f.store.resolve_playback(user, &e02, None, None).unwrap();

    let second = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                2_300_000,
                PlaybackEventType::Stopped,
            ),
            3_000,
        )
        .unwrap();
    assert_eq!(
        second,
        EventOutcome::IgnoredStale,
        "a repeated Stop writes nothing"
    );

    let beyond = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                10_000,
                PlaybackEventType::Progress,
            ),
            4_000,
        )
        .unwrap();
    assert_eq!(
        beyond,
        EventOutcome::IgnoredStale,
        "a closed session cannot be revived"
    );

    let unit_after_ignored = f.store.unit_state(user, f.media_id, 1, 2).unwrap();
    assert_eq!(
        unit_after_ignored, unit_after_first,
        "a repeated Stop and a post-close Progress must not write any unit fact"
    );
    let played = f.store.resolve_playback(user, &e02, None, None).unwrap();
    assert_eq!(
        played.status,
        ResumeStatus::AlreadyPlayed,
        "positive control: the single Stop really completed the unit"
    );
    assert_eq!(
        played, resume_after_first,
        "the readable resume record is the one the single Stop produced"
    );
    assert_eq!(
        unit_position(&f, 2),
        Some(0),
        "an already-played unit restarts at 0"
    );
}

#[test]
fn admin_ended_session_is_archived_without_writing_unit_facts() {
    let (_tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();
    let e02 = episode(&f, 2);
    let session = issue(&f, &e02, &f.file);
    let first = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                5_000,
                PlaybackEventType::Progress,
            ),
            2_000,
        )
        .unwrap();
    assert_eq!(first, EventOutcome::Applied);
    assert_eq!(
        unit_position(&f, 2),
        Some(5_000),
        "the session is live and moving"
    );

    // The admin ends this device's playback while the session is still live.
    let mut live = f.store.get_session(user, "device_web").unwrap().unwrap();
    live.admin_ended = true;
    live.last_report_at = 2_500;
    f.store.upsert_session(&live).unwrap();

    // A late heartbeat for the ended session archives the live row instead of moving the
    // watch position: the store owns the admin-ended guard, the API no longer pre-empts it.
    let logs_before = f
        .store
        .list_logs(10, None, None, None, 10_000, Some(user))
        .unwrap()
        .len();
    let ended = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                9_000,
                PlaybackEventType::Progress,
            ),
            3_000,
        )
        .unwrap();
    // 类不变式判别:返回 `Applied` 就必须真的有写入(`IgnoredStale` ⟺ 零写入)。
    // 这里「有写入」= 归档 live 行 + 落一条 play log,两者都在下面被断言。
    assert_eq!(
        ended,
        EventOutcome::Applied,
        "ending the session is a write, so the event is not stale"
    );
    assert!(
        f.store.get_session(user, "device_web").unwrap().is_none(),
        "the admin-ended live row is archived"
    );
    let archived = f
        .store
        .play_session_binding(user, "device_web", &session.0)
        .unwrap()
        .unwrap();
    assert_eq!(
        archived.state,
        PlaySessionIdState::Closed,
        "archiving closes the id row"
    );
    assert_eq!(
        f.store
            .list_logs(10, None, None, None, 10_000, Some(user))
            .unwrap()
            .len(),
        logs_before + 1,
        "the archive of an admin-ended session writes exactly one play log"
    );
    assert_eq!(
        unit_position(&f, 2),
        Some(5_000),
        "an admin-ended heartbeat must not write the position it reports"
    );

    // The close archived `admin_ended`, so the session cannot be revived by a later event.
    let again = f
        .store
        .commit_playback_event(
            &event(
                &f,
                e02.clone(),
                (&session.0, session.1),
                &f.file,
                11_000,
                PlaybackEventType::Progress,
            ),
            3_500,
        )
        .unwrap();
    assert_eq!(
        again,
        EventOutcome::IgnoredStale,
        "the closed session stays closed"
    );
    assert_eq!(unit_position(&f, 2), Some(5_000));
}

#[test]
fn refused_activation_of_a_superseded_pending_session_writes_nothing() {
    let (tmp, f) = seed();
    let user: UserId = USER.parse().unwrap();
    let device = "device_web";
    let e01 = episode(&f, 1);

    let older = issue_on(&f, device, &e01, &f.file);
    let newer = issue_on(&f, device, &e01, &f.file);
    assert!(
        newer.1 > older.1,
        "generations are strictly increasing per device"
    );
    assert!(
        f.store
            .activate_play_session(user, device, &newer.0)
            .unwrap()
    );
    let newer_row = f
        .store
        .play_session_binding(user, device, &newer.0)
        .unwrap()
        .unwrap();
    assert_eq!(
        newer_row.state,
        PlaySessionIdState::Active,
        "activation really activates the row"
    );

    // 夹具:把已被取代的行放回 `pending`。
    // 该前置状态在本仓全部写入路径上不可达:`issue_play_session` 的 generation 恒为
    // `MAX(generation)+1`,任何一次成功激活都会把 generation 更小的 `pending` 行置为
    // `closed`,且没有第二条 INSERT、也没有把行退回 `pending` 的语句。这里直接构造它,
    // 只为钉住「激活被拒绝 ⇒ 零写入」这一不变式;断言只读公开接口 `play_session_binding`。
    let raw = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let reverted = raw
        .execute(
            "UPDATE playback_session_ids SET state = 'pending' WHERE play_session_id = ?1",
            rusqlite::params![older.0],
        )
        .unwrap();
    assert_eq!(
        reverted, 1,
        "the fixture reverts exactly the superseded row"
    );
    drop(raw);
    let older_before = f
        .store
        .play_session_binding(user, device, &older.0)
        .unwrap();
    let newer_before = f
        .store
        .play_session_binding(user, device, &newer.0)
        .unwrap();

    assert!(
        !f.store
            .activate_play_session(user, device, &older.0)
            .unwrap(),
        "a superseded pending session is refused"
    );
    let binding = f
        .store
        .play_session_binding(user, device, &older.0)
        .unwrap()
        .expect("the refused row must still exist");
    assert_eq!(
        binding.state,
        PlaySessionIdState::Pending,
        "a refused activation must not close the row it refuses"
    );
    assert_eq!(
        (
            f.store
                .play_session_binding(user, device, &older.0)
                .unwrap(),
            f.store
                .play_session_binding(user, device, &newer.0)
                .unwrap()
        ),
        (older_before, newer_before),
        "a refused activation writes nothing on either row"
    );
}
