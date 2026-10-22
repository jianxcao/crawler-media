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
use store::playback::{
    BoundPlaybackEvent, EventOutcome, PlaySessionIdState, PlaybackEventType, ResumeStatus,
};
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
        device_name: None,
        client_version: None,
        play_method: None,
        event_type,
    }
}

fn unit_position(f: &Fixture, number: i32) -> Option<i64> {
    let user: UserId = USER.parse().unwrap();
    let item = episode(f, number);
    let resume = f
        .store
        .resolve_playback(user, &item, None, None)
        .expect("a failed read must not look like a missing stored position");
    Some(resume.start_ms)
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
        e01_after,
        e01_before,
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
    assert_eq!(e01_before.status, ResumeStatus::NoProgress, "E01 was never played");

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
        e01_after,
        e01_before,
        "the late E01 event must not write any field of E01's resume record"
    );

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
    assert_eq!(resume.status, ResumeStatus::Compatible, "the bound source is the issued one");
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
    let user: UserId = USER.parse().unwrap();
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
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 2_300_000, PlaybackEventType::Stopped),
            3_000,
        )
        .unwrap();
    assert_eq!(second, EventOutcome::IgnoredStale, "a repeated Stop writes nothing");

    let beyond = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 10_000, PlaybackEventType::Progress),
            4_000,
        )
        .unwrap();
    assert_eq!(beyond, EventOutcome::IgnoredStale, "a closed session cannot be revived");

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
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 5_000, PlaybackEventType::Progress),
            2_000,
        )
        .unwrap();
    assert_eq!(first, EventOutcome::Applied);
    assert_eq!(unit_position(&f, 2), Some(5_000), "the session is live and moving");

    // The admin ends this device's playback while the session is still live.
    let mut live = f.store.get_session(user, "device_web").unwrap().unwrap();
    live.admin_ended = true;
    live.last_report_at = 2_500;
    f.store.upsert_session(&live).unwrap();

    // A late heartbeat for the ended session archives the live row instead of moving the
    // watch position: the store owns the admin-ended guard, the API no longer pre-empts it.
    let logs_before = f.store.list_logs(10, None, None, None, 10_000, Some(user)).unwrap().len();
    let ended = f
        .store
        .commit_playback_event(
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 9_000, PlaybackEventType::Progress),
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
    let archived = f.store.play_session_binding(user, "device_web", &session.0).unwrap().unwrap();
    assert_eq!(archived.state, PlaySessionIdState::Closed, "archiving closes the id row");
    assert_eq!(
        f.store.list_logs(10, None, None, None, 10_000, Some(user)).unwrap().len(),
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
            &event(&f, e02.clone(), (&session.0, session.1), &f.file, 11_000, PlaybackEventType::Progress),
            3_500,
        )
        .unwrap();
    assert_eq!(again, EventOutcome::IgnoredStale, "the closed session stays closed");
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
    assert!(newer.1 > older.1, "generations are strictly increasing per device");
    assert!(f.store.activate_play_session(user, device, &newer.0).unwrap());
    let newer_row = f.store.play_session_binding(user, device, &newer.0).unwrap().unwrap();
    assert_eq!(newer_row.state, PlaySessionIdState::Active, "activation really activates the row");

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
    assert_eq!(reverted, 1, "the fixture reverts exactly the superseded row");
    drop(raw);
    let older_before = f.store.play_session_binding(user, device, &older.0).unwrap();
    let newer_before = f.store.play_session_binding(user, device, &newer.0).unwrap();

    assert!(
        !f.store.activate_play_session(user, device, &older.0).unwrap(),
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
            f.store.play_session_binding(user, device, &older.0).unwrap(),
            f.store.play_session_binding(user, device, &newer.0).unwrap()
        ),
        (older_before, newer_before),
        "a refused activation writes nothing on either row"
    );
}

// ---------------------------------------------------------------------------------------------
// Watch-time accounting: a `Progress` event may claim elapsed wall time only while playback is
// actually running. `paused` and `Stopped` are facts the event already carries; these tests pin
// them at the public `commit_playback_event` seam, so no separate classifier is needed.
// ---------------------------------------------------------------------------------------------

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
    assert_eq!(watched_ms(&f, "device_web"), 0, "the first heartbeat has no earlier interval");

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
    assert_eq!(row.position_ms, 6_000, "the pause still records where playback stopped");
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
    for (device, position_ms, watched) in
        [("device_budget_in", 8_000, 1_000), ("device_budget_out", 8_001, 0)]
    {
        let s = issue_on(&f, device, &episode(&f, 1), &f.file);
        assert_eq!(
            heartbeat(&f, device, (&s.0, s.1), 5_000, false, 2_000),
            EventOutcome::Applied
        );
        assert_eq!(
            heartbeat(&f, device, (&s.0, s.1), position_ms, false, 3_000),
            EventOutcome::Applied
        );
        assert_eq!(watched_ms(&f, device), watched, "1 s allows exactly 3_000 ms of movement");
    }
}

#[test]
fn seek_disguised_as_progress_claims_no_watch_time_in_either_state() {
    let (_tmp, f) = seed();
    let item = episode(&f, 1);

    let playing = issue_on(&f, "device_seek_playing", &item, &f.file);
    assert_eq!(
        heartbeat(&f, "device_seek_playing", (&playing.0, playing.1), 5_000, false, 2_000),
        EventOutcome::Applied
    );
    assert_eq!(
        heartbeat(&f, "device_seek_playing", (&playing.0, playing.1), 105_000, false, 3_000),
        EventOutcome::Applied
    );
    assert_eq!(watched_ms(&f, "device_seek_playing"), 0, "a seek is not watching");

    let paused = issue_on(&f, "device_seek_paused", &item, &f.file);
    assert_eq!(
        heartbeat(&f, "device_seek_paused", (&paused.0, paused.1), 5_000, false, 2_000),
        EventOutcome::Applied
    );
    assert_eq!(
        heartbeat(&f, "device_seek_paused", (&paused.0, paused.1), 105_000, true, 3_000),
        EventOutcome::Applied
    );
    assert_eq!(watched_ms(&f, "device_seek_paused"), 0, "a paused seek claims no watch time either");
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
    let mut stop = event(&f, item, (&s.0, s.1), &f.file, 6_000, PlaybackEventType::Stopped);
    stop.paused = true;
    assert_eq!(f.store.commit_playback_event(&stop, 3_000).unwrap(), EventOutcome::Applied);
    assert!(
        f.store.get_session(user, "device_web").unwrap().is_none(),
        "a stop archives the live row"
    );
    let archived = f.store.play_session_binding(user, "device_web", &s.0).unwrap().unwrap();
    assert_eq!(archived.state, PlaySessionIdState::Closed, "a stop closes the id row");

    let logs = f.store.list_logs(10, None, None, None, 3_000, Some(user)).unwrap();
    assert_eq!(logs.len(), 1, "the stop writes exactly one play log");
    let log = logs.first().expect("the stop writes one play log");
    assert_eq!(log.end_position_ms, 6_000, "the stop's final position is archived");
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
    assert_eq!(heartbeat(&f, "device_web", (&a.0, a.1), 0, false, 2_000), EventOutcome::Applied);
    assert_eq!(heartbeat(&f, "device_web", (&a.0, a.1), 10_000, false, 12_000), EventOutcome::Applied);
    assert_eq!(watched_ms(&f, "device_web"), 10_000, "positive control: A watched 10s");

    let b = issue(&f, &episode(&f, 1), &f.file);
    assert_eq!(heartbeat(&f, "device_web", (&b.0, b.1), 10_000, false, 13_000), EventOutcome::Applied);
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
