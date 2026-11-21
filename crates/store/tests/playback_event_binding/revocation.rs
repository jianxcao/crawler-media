use std::sync::{Arc, Barrier};

use domain::{
    LibraryFile, MediaId, PhysicalSourceId, TypedItemId, UnitKey, User, UserId, UserRole,
};
use store::Store;
use store::playback::{BoundPlaybackEvent, EventOutcome, PlaybackEventType};

use super::seed;

struct MemberSession<'a> {
    user_id: UserId,
    device: &'a str,
    sid: String,
    generation: i64,
}

fn member_event(
    f_media: MediaId,
    file: &LibraryFile,
    session: &MemberSession<'_>,
    seq: i64,
    pos: i64,
    event_type: PlaybackEventType,
) -> BoundPlaybackEvent {
    BoundPlaybackEvent {
        user_id: session.user_id,
        device_id: session.device.into(),
        session_id: session.sid.clone(),
        generation: session.generation,
        event_sequence: Some(seq),
        item: TypedItemId::Unit {
            media: f_media,
            key: UnitKey::new(1, 1),
        },
        source_id: PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        },
        source_version: file.version.source_version.clone(),
        position_ms: Some(pos),
        duration_ms: Some(2_400_000),
        audio_track: None,
        subtitle_track: None,
        paused: false,
        playback_rate: 1.0,
        rate_bps: 0,
        client: Some("Web".into()),
        device_name: None,
        client_version: None,
        play_method: None,
        event_type,
    }
}

fn issue_member_session<'a>(
    store: &Store,
    user_id: UserId,
    device: &'a str,
    media_id: MediaId,
    file: &LibraryFile,
) -> MemberSession<'a> {
    let sid = store
        .issue_play_session(
            user_id,
            device,
            file.file_id,
            media_id,
            UnitKey::new(1, 1),
            &file.version,
            1_000,
        )
        .unwrap();
    let generation = store
        .play_session_binding(user_id, device, &sid)
        .unwrap()
        .unwrap()
        .generation;
    MemberSession {
        user_id,
        device,
        sid,
        generation,
    }
}

fn seed_two_members(store: &Store, library_id: &str) -> (UserId, UserId) {
    let (member, kept) = (UserId::new(), UserId::new());
    for (id, login) in [(member, "revoked_member"), (kept, "kept_member")] {
        store
            .insert_user(&User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            })
            .unwrap();
    }
    store
        .update_library(
            library_id,
            None,
            None,
            Some(("selected", true, &[member, kept])),
        )
        .unwrap();
    (member, kept)
}

#[test]
fn revoked_member_queued_playback_event_is_ignored_stale() {
    let (_tmp, f) = seed();
    let store = Arc::new(f.store);
    let (member, kept) = seed_two_members(&store, &f.library_id);
    let member_session = issue_member_session(&store, member, "dev_member", f.media_id, &f.file);
    let kept_session = issue_member_session(&store, kept, "dev_kept", f.media_id, &f.file);

    let ev1 = member_event(
        f.media_id,
        &f.file,
        &member_session,
        1,
        5_000,
        PlaybackEventType::Progress,
    );
    assert_eq!(
        store.commit_playback_event(&ev1, 2_000).unwrap(),
        EventOutcome::Applied
    );
    let unit_before = store.unit_state(member, f.media_id, 1, 1).unwrap();
    let logs_before = store
        .list_logs(20, None, None, None, 10_000, Some(member))
        .unwrap();

    let (pre_writer, revoked) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let stop_event = member_event(
        f.media_id,
        &f.file,
        &member_session,
        2,
        2_300_000,
        PlaybackEventType::Stopped,
    );
    let outcome = std::thread::scope(|scope| {
        let (store_ev, pre, rev) = (
            Arc::clone(&store),
            Arc::clone(&pre_writer),
            Arc::clone(&revoked),
        );
        let handle = scope.spawn(move || {
            store_ev
                .commit_playback_event_observed(&stop_event, 3_000, || {
                    pre.wait();
                    rev.wait();
                })
                .unwrap()
        });
        pre_writer.wait();
        store
            .update_library(&f.library_id, None, None, Some(("selected", true, &[kept])))
            .unwrap();
        revoked.wait();
        handle.join().unwrap()
    });

    assert_eq!(outcome, EventOutcome::IgnoredStale);
    assert_eq!(
        store.unit_state(member, f.media_id, 1, 1).unwrap(),
        unit_before
    );
    assert_eq!(
        store
            .list_logs(20, None, None, None, 10_000, Some(member))
            .unwrap(),
        logs_before
    );
    assert_eq!(
        store
            .play_session_binding(member, "dev_member", &member_session.sid)
            .unwrap()
            .unwrap()
            .last_event_sequence,
        Some(1)
    );

    // Positive control: a member who kept access still commits normally.
    let kept_ev = member_event(
        f.media_id,
        &f.file,
        &kept_session,
        1,
        12_000,
        PlaybackEventType::Progress,
    );
    assert_eq!(
        store.commit_playback_event(&kept_ev, 4_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(
        store
            .unit_state(kept, f.media_id, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        12_000
    );
}
