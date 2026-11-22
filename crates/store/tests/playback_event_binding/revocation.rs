use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use domain::{
    FilePresence, FilePublication, LibraryFile, Media, MediaId, MediaKind, PhysicalSourceId,
    TypedItemId, UnitKey, User, UserId, UserRole,
};
use store::playback::session_ids::PlaybackRenewalError;
use store::playback::{
    BoundPlaybackEvent, EventOutcome, MediaMergeRequest, PlaySessionIdState, PlaybackEventType,
    PlaybackMutation,
};
use store::{Store, StoreError};

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

    struct SignalOnDrop(Option<mpsc::SyncSender<()>>);
    impl Drop for SignalOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.try_send(());
            }
        }
    }

    let (pre_tx, pre_rx) = mpsc::sync_channel::<()>(1);
    let (rev_tx, rev_rx) = mpsc::sync_channel::<()>(1);
    let stop_event = member_event(
        f.media_id,
        &f.file,
        &member_session,
        2,
        2_300_000,
        PlaybackEventType::Stopped,
    );
    let outcome = std::thread::scope(|scope| {
        let store_ev = Arc::clone(&store);
        let rev_guard = SignalOnDrop(Some(rev_tx.clone()));
        let handle = scope.spawn(move || {
            let _pre_guard = SignalOnDrop(Some(pre_tx.clone()));
            store_ev
                .commit_playback_event_observed(&stop_event, 3_000, || {
                    let _ = pre_tx.try_send(());
                    rev_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("revocation signal timed out");
                })
                .unwrap()
        });
        pre_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker did not reach before_writer hook");
        store
            .update_library(&f.library_id, None, None, Some(("selected", true, &[kept])))
            .unwrap();
        let _ = rev_tx.try_send(());
        drop(rev_guard);
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

fn insert_test_media(store: &Store, id: MediaId, title: &str) {
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Tv,
            title: title.into(),
            year: Some(2024),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
}

#[test]
fn mutate_playback_enforces_library_visibility_and_published_state() {
    let (_tmp, f) = seed();
    let admin = UserId::new();
    let member = UserId::new();
    for (id, login, role) in [
        (admin, "vis_admin", UserRole::Admin),
        (member, "vis_member", UserRole::Member),
    ] {
        f.store
            .insert_user(&User {
                id,
                login: login.into(),
                enabled: true,
                role,
            })
            .unwrap();
    }

    // Hidden to admin (`admin_visible = false`) -> Missing
    f.store
        .update_library(&f.library_id, None, None, Some(("everyone", false, &[])))
        .unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(admin, 1, 1, Some(true), None),
            2_000,
        ),
        Err(StoreError::Missing(_))
    ));
    assert!(f.store.unit_rows(admin, f.media_id).unwrap().is_empty());

    // Selected excluding member -> Missing
    f.store
        .update_library(&f.library_id, None, None, Some(("selected", true, &[])))
        .unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            2_000,
        ),
        Err(StoreError::Missing(_))
    ));
    assert!(f.store.unit_rows(member, f.media_id).unwrap().is_empty());

    // Unowned media -> Missing
    f.store
        .update_library(&f.library_id, None, None, Some(("everyone", true, &[])))
        .unwrap();
    let unowned = MediaId::new();
    insert_test_media(&f.store, unowned, "Unowned");
    assert!(matches!(
        f.store.mutate_playback(
            unowned,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            2_000,
        ),
        Err(StoreError::Missing(_))
    ));

    // Pending-only media -> Missing
    store::test_support::force_file_states(
        &f.store,
        f.file.file_id,
        FilePresence::Present,
        FilePublication::Pending,
    )
    .unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            2_900,
        ),
        Err(StoreError::Missing(_))
    ));

    // Library under removal -> Missing
    store::test_support::force_file_states(
        &f.store,
        f.file.file_id,
        FilePresence::Present,
        FilePublication::Published,
    )
    .unwrap();
    f.store.begin_library_removal(&f.library_id).unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            3_000,
        ),
        Err(StoreError::Missing(_))
    ));
}

#[test]
fn mutate_playback_enforces_actor_and_protected_precedence() {
    let (_tmp, f) = seed();
    let member = UserId::new();
    let removed = UserId::new();
    let disabled = UserId::new();
    for (id, login) in [
        (member, "vis_member2"),
        (removed, "vis_removed"),
        (disabled, "vis_disabled"),
    ] {
        f.store
            .insert_user(&User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            })
            .unwrap();
    }

    // Inactive actors -> Protected
    let unknown = UserId::new();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(unknown, 1, 1, Some(true), None),
            2_000,
        ),
        Err(StoreError::Protected(_))
    ));
    f.store
        .save_user(&User {
            id: disabled,
            login: "vis_disabled".into(),
            enabled: false,
            role: UserRole::Member,
        })
        .unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(disabled, 1, 1, Some(true), None),
            2_000,
        ),
        Err(StoreError::Protected(_))
    ));
    f.store
        .app_write(|app| app.accept_user_removal(removed, &format!("user_removal.{removed}"), "{}"))
        .unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(removed, 1, 1, Some(true), None),
            2_000,
        ),
        Err(StoreError::Protected(_))
    ));

    // Merging / retired loser preserves Protected before visibility
    let loser = MediaId::new();
    insert_test_media(&f.store, loser, "Loser");
    let op = f
        .store
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000b1".into(),
                survivor: f.media_id,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: BTreeMap::from([("tmdb".into(), f.media_id.to_string())]),
            },
            2_500,
        )
        .unwrap();
    assert!(matches!(
        f.store.mutate_playback(
            loser,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            2_600,
        ),
        Err(StoreError::Protected(_))
    ));
    assert_eq!(
        f.store.advance_media_merge(&op, 2_700).unwrap().state,
        "succeeded"
    );
    let err = f
        .store
        .mutate_playback(
            loser,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            2_800,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Protected(_)), "{err}");
    assert!(err.to_string().contains(&f.media_id.to_string()), "{err}");
}

fn issue_and_activate_session<'a>(
    store: &Store,
    user: UserId,
    device: &'a str,
    media_id: MediaId,
    file: &LibraryFile,
    pos: i64,
) -> MemberSession<'a> {
    let s = issue_member_session(store, user, device, media_id, file);
    let ev = member_event(media_id, file, &s, 1, pos, PlaybackEventType::Progress);
    assert_eq!(
        store.commit_playback_event(&ev, 2_000).unwrap(),
        EventOutcome::Applied
    );
    s
}

#[test]
fn revoke_device_sessions_archives_live_closes_pending_and_preserves_other_devices() {
    let (_tmp, f) = seed();
    let member = UserId::new();
    f.store
        .insert_user(&User {
            id: member,
            login: "rev_sess_member".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    let resolved = f
        .store
        .resolve_playback(
            member,
            &TypedItemId::Unit {
                media: f.media_id,
                key: UnitKey::new(1, 1),
            },
            None,
            None,
        )
        .unwrap();

    let s_live =
        issue_and_activate_session(&f.store, member, "dev_target", f.media_id, &f.file, 15_000);
    let s_pending = issue_member_session(&f.store, member, "dev_target", f.media_id, &f.file);
    let s_other =
        issue_and_activate_session(&f.store, member, "dev_other", f.media_id, &f.file, 25_000);
    let s_other_pending = issue_member_session(&f.store, member, "dev_other", f.media_id, &f.file);

    assert!(
        f.store
            .revoke_device_sessions(member, "dev_target", 5_000)
            .unwrap()
    );
    assert!(f.store.get_session(member, "dev_target").unwrap().is_none());
    assert!(
        f.store
            .device_was_admin_ended(member, "dev_target", 5_000, 60)
            .unwrap()
    );
    assert!(
        f.store
            .archived_play_session_was_admin_ended(
                member,
                "dev_target",
                &s_live.sid,
                s_live.generation,
                5_000,
                60,
            )
            .unwrap()
    );
    for sid in [&s_live.sid, &s_pending.sid] {
        assert_eq!(
            f.store
                .play_session_binding(member, "dev_target", sid)
                .unwrap()
                .unwrap()
                .state,
            PlaySessionIdState::Closed
        );
    }
    assert!(matches!(
        f.store
            .renew_play_session(member, "dev_target", &s_live.sid, &resolved, 5_001),
        Err(PlaybackRenewalError::EndedByAdmin)
    ));

    // Other device untouched
    assert!(f.store.get_session(member, "dev_other").unwrap().is_some());
    assert_eq!(
        f.store
            .play_session_binding(member, "dev_other", &s_other.sid)
            .unwrap()
            .unwrap()
            .state,
        PlaySessionIdState::Active
    );
    assert_eq!(
        f.store
            .play_session_binding(member, "dev_other", &s_other_pending.sid)
            .unwrap()
            .unwrap()
            .state,
        PlaySessionIdState::Pending
    );
    assert!(
        !f.store
            .device_was_admin_ended(member, "dev_other", 5_000, 60)
            .unwrap()
    );
}

#[test]
fn revoke_device_sessions_handles_pending_only_and_idempotency() {
    let (_tmp, f) = seed();
    let member = UserId::new();
    f.store
        .insert_user(&User {
            id: member,
            login: "rev_sess_pending".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    let s_only_pending =
        issue_member_session(&f.store, member, "dev_pending_only", f.media_id, &f.file);
    let logs_before = f
        .store
        .list_logs(20, None, None, None, 10_000, Some(member))
        .unwrap()
        .len();
    assert!(
        f.store
            .revoke_device_sessions(member, "dev_pending_only", 5_000)
            .unwrap()
    );
    assert_eq!(
        f.store
            .play_session_binding(member, "dev_pending_only", &s_only_pending.sid)
            .unwrap()
            .unwrap()
            .state,
        PlaySessionIdState::Closed
    );
    assert_eq!(
        f.store
            .list_logs(20, None, None, None, 10_000, Some(member))
            .unwrap()
            .len(),
        logs_before
    );
    assert!(
        !f.store
            .revoke_device_sessions(member, "dev_pending_only", 5_001)
            .unwrap()
    );
    assert!(
        !f.store
            .revoke_device_sessions(member, "dev_unknown", 5_001)
            .unwrap()
    );
}
