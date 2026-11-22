use std::sync::{Arc, mpsc};
use std::time::Duration;

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaId, MediaKind,
    PhysicalSourceId, QualitySource, TypedItemId, UnitKey, User, UserId, UserRole,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::session_ids::PlaybackRenewalError;
use store::playback::{BoundPlaybackEvent, EventOutcome, PlaybackEventType, PlaybackMutation};
use store::{Db, Store, StoreError};

use super::seed;

struct SignalOnDrop(Option<mpsc::SyncSender<()>>);

impl Drop for SignalOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.try_send(());
        }
    }
}

fn seed_active_member(store: &Store, library_id: &str, login: &str) -> UserId {
    let user = UserId::new();
    store
        .insert_user(&User {
            id: user,
            login: login.into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    store
        .update_library(library_id, None, None, Some(("selected", true, &[user])))
        .unwrap();
    user
}

fn issue_and_prime_progress(
    store: &Store,
    user_id: UserId,
    device: &str,
    media_id: MediaId,
    file: &LibraryFile,
) -> (String, i64) {
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
    let ev = make_event(
        user_id,
        device,
        &sid,
        generation,
        1,
        media_id,
        file,
        5_000,
        PlaybackEventType::Progress,
    );
    assert_eq!(
        store.commit_playback_event(&ev, 2_000).unwrap(),
        EventOutcome::Applied
    );
    (sid, generation)
}

#[allow(clippy::too_many_arguments)]
fn make_event(
    user_id: UserId,
    device_id: &str,
    session_id: &str,
    generation: i64,
    seq: i64,
    media: MediaId,
    file: &LibraryFile,
    pos: i64,
    event_type: PlaybackEventType,
) -> BoundPlaybackEvent {
    BoundPlaybackEvent {
        user_id,
        device_id: device_id.into(),
        session_id: session_id.into(),
        generation,
        event_sequence: Some(seq),
        item: TypedItemId::Unit {
            media,
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

struct RaceSession<'a> {
    user: UserId,
    device: &'a str,
    sid: &'a str,
    generation: i64,
}

fn assert_before_writer_race_stale(
    store: Arc<Store>,
    media_id: MediaId,
    file: &LibraryFile,
    target: RaceSession<'_>,
    mutate_app: impl FnOnce(&Store),
) {
    let RaceSession {
        user,
        device,
        sid,
        generation,
    } = target;
    let unit_before = store.unit_state(user, media_id, 1, 1).unwrap();
    let logs_before = store
        .list_logs(20, None, None, None, 10_000, Some(user))
        .unwrap();
    let (pre_tx, pre_rx) = mpsc::sync_channel::<()>(1);
    let (go_tx, go_rx) = mpsc::sync_channel::<()>(1);
    let stop_event = make_event(
        user,
        device,
        sid,
        generation,
        2,
        media_id,
        file,
        2_300_000,
        PlaybackEventType::Stopped,
    );
    let outcome = std::thread::scope(|scope| {
        let store_ev = Arc::clone(&store);
        let go_guard = SignalOnDrop(Some(go_tx.clone()));
        let handle = scope.spawn(move || {
            let _pre_guard = SignalOnDrop(Some(pre_tx.clone()));
            store_ev
                .commit_playback_event_observed(&stop_event, 3_000, || {
                    let _ = pre_tx.try_send(());
                    go_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("race gate signal timed out");
                })
                .unwrap()
        });
        pre_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker did not reach before_writer hook");
        mutate_app(&store);
        let _ = go_tx.try_send(());
        drop(go_guard);
        handle.join().unwrap()
    });
    assert_eq!(outcome, EventOutcome::IgnoredStale);
    assert_eq!(store.unit_state(user, media_id, 1, 1).unwrap(), unit_before);
    assert_eq!(
        store
            .list_logs(20, None, None, None, 10_000, Some(user))
            .unwrap(),
        logs_before
    );
    assert_eq!(
        store
            .play_session_binding(user, device, sid)
            .unwrap()
            .unwrap()
            .last_event_sequence,
        Some(1)
    );
    assert_eq!(
        store
            .get_session(user, device)
            .unwrap()
            .unwrap()
            .position_ms,
        5_000
    );
}

#[test]
fn before_writer_race_with_revoke_device_is_ignored_stale() {
    let (_tmp, f) = seed();
    let store = Arc::new(f.store);
    let member = seed_active_member(&store, &f.library_id, "race_revoked_dev");
    store.set_user_token(member, "tok-race").unwrap();
    store
        .bind_playback_credential(member, "dev_race", "tok-race")
        .unwrap();
    let (sid, generation) =
        issue_and_prime_progress(&store, member, "dev_race", f.media_id, &f.file);
    assert_before_writer_race_stale(
        store,
        f.media_id,
        &f.file,
        RaceSession {
            user: member,
            device: "dev_race",
            sid: &sid,
            generation,
        },
        |s| s.revoke_device(member, "dev_race").unwrap(),
    );
}

#[test]
fn before_writer_race_with_disable_user_is_ignored_stale() {
    let (_tmp, f) = seed();
    let store = Arc::new(f.store);
    let member = seed_active_member(&store, &f.library_id, "race_disabled_user");
    let (sid, generation) =
        issue_and_prime_progress(&store, member, "dev_dis", f.media_id, &f.file);
    assert_before_writer_race_stale(
        store,
        f.media_id,
        &f.file,
        RaceSession {
            user: member,
            device: "dev_dis",
            sid: &sid,
            generation,
        },
        |s| {
            s.save_user(&User {
                id: member,
                login: "race_disabled_user".into(),
                enabled: false,
                role: UserRole::Member,
            })
            .unwrap();
        },
    );
}

#[test]
fn before_writer_race_with_accept_user_removal_is_ignored_stale() {
    let (_tmp, f) = seed();
    let store = Arc::new(f.store);
    let member = seed_active_member(&store, &f.library_id, "race_removing_user");
    let (sid, generation) =
        issue_and_prime_progress(&store, member, "dev_rem", f.media_id, &f.file);
    assert_before_writer_race_stale(
        store,
        f.media_id,
        &f.file,
        RaceSession {
            user: member,
            device: "dev_rem",
            sid: &sid,
            generation,
        },
        |s| {
            s.app_write(|app| {
                app.accept_user_removal(member, &format!("user_removal.{member}"), "{}")
            })
            .unwrap();
        },
    );
}

#[test]
fn before_writer_race_with_role_promotion_on_hidden_admin_library_is_ignored_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Hidden Admin Show".into(),
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
    let lib = store
        .create_library(MediaKind::Tv, "HiddenTV", &["/htv"], "everyone", false, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == lib.id)
        .unwrap()
        .id;
    let file = LibraryFile {
        file_id: "00000000-0000-0000-0000-000000000022".parse().unwrap(),
        library_id: lib.id.parse().unwrap(),
        root_id,
        media_id,
        kind: MediaKind::Tv,
        path: "Hidden Admin Show/S01E01.mkv".into(),
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
        source_digest: Some("h2".into()),
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
        tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    let member = UserId::new();
    store
        .insert_user(&User {
            id: member,
            login: "promoted".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    let (sid, generation) = issue_and_prime_progress(&store, member, "dev_prom", media_id, &file);
    assert_before_writer_race_stale(
        store,
        media_id,
        &file,
        RaceSession {
            user: member,
            device: "dev_prom",
            sid: &sid,
            generation,
        },
        |s| {
            s.save_user(&User {
                id: member,
                login: "promoted".into(),
                enabled: true,
                role: UserRole::Admin,
            })
            .unwrap();
        },
    );
}

fn assert_issue_and_renew_protected(
    store: &Store,
    user: UserId,
    device: &str,
    media_id: MediaId,
    file: &LibraryFile,
    prev_sid: &str,
    resolved: &store::ResolvedResume,
) {
    assert!(matches!(
        store.issue_play_session(
            user,
            device,
            file.file_id,
            media_id,
            UnitKey::new(1, 1),
            &file.version,
            1_001,
        ),
        Err(StoreError::Protected(_))
    ));
    assert!(matches!(
        store.renew_play_session(user, device, prev_sid, resolved, 1_002),
        Err(PlaybackRenewalError::Store(StoreError::Protected(_)))
    ));
}

#[test]
fn issue_and_renew_reject_revoked_device_disabled_user_and_removing_user() {
    let (_tmp, f) = seed();
    let item = TypedItemId::Unit {
        media: f.media_id,
        key: UnitKey::new(1, 1),
    };

    // (a) Revoked device
    let dev_user = seed_active_member(&f.store, &f.library_id, "actor_rev_dev");
    f.store.set_user_token(dev_user, "tok-a").unwrap();
    f.store
        .bind_playback_credential(dev_user, "dev_a", "tok-a")
        .unwrap();
    let resolved_a = f
        .store
        .resolve_playback(dev_user, &item, None, None)
        .unwrap();
    let orig_a = f
        .store
        .issue_play_session(
            dev_user,
            "dev_a",
            f.file.file_id,
            f.media_id,
            UnitKey::new(1, 1),
            &f.file.version,
            1_000,
        )
        .unwrap();
    f.store.revoke_device(dev_user, "dev_a").unwrap();
    assert_issue_and_renew_protected(
        &f.store,
        dev_user,
        "dev_a",
        f.media_id,
        &f.file,
        &orig_a,
        &resolved_a,
    );
    assert_eq!(
        f.store
            .play_session_binding(dev_user, "dev_a", &orig_a)
            .unwrap()
            .unwrap()
            .generation,
        1
    );

    // (b) Disabled user
    let dis_user = seed_active_member(&f.store, &f.library_id, "actor_dis");
    let resolved_b = f
        .store
        .resolve_playback(dis_user, &item, None, None)
        .unwrap();
    let orig_b = f
        .store
        .issue_play_session(
            dis_user,
            "dev_b",
            f.file.file_id,
            f.media_id,
            UnitKey::new(1, 1),
            &f.file.version,
            1_000,
        )
        .unwrap();
    f.store
        .save_user(&User {
            id: dis_user,
            login: "actor_dis".into(),
            enabled: false,
            role: UserRole::Member,
        })
        .unwrap();
    assert_issue_and_renew_protected(
        &f.store,
        dis_user,
        "dev_b",
        f.media_id,
        &f.file,
        &orig_b,
        &resolved_b,
    );

    // (c) Removing user
    let rem_user = seed_active_member(&f.store, &f.library_id, "actor_rem");
    let resolved_c = f
        .store
        .resolve_playback(rem_user, &item, None, None)
        .unwrap();
    let orig_c = f
        .store
        .issue_play_session(
            rem_user,
            "dev_c",
            f.file.file_id,
            f.media_id,
            UnitKey::new(1, 1),
            &f.file.version,
            1_000,
        )
        .unwrap();
    f.store
        .app_write(|app| {
            app.accept_user_removal(rem_user, &format!("user_removal.{rem_user}"), "{}")
        })
        .unwrap();
    assert_issue_and_renew_protected(
        &f.store,
        rem_user,
        "dev_c",
        f.media_id,
        &f.file,
        &orig_c,
        &resolved_c,
    );
}

fn assert_verified_library_writes_succeed(
    store: &Store,
    member: UserId,
    media_id: MediaId,
    file: &LibraryFile,
    sid: &str,
    generation: i64,
    resolved: &store::ResolvedResume,
) {
    let ev = make_event(
        member,
        "dev_maint",
        sid,
        generation,
        2,
        media_id,
        file,
        8_000,
        PlaybackEventType::Progress,
    );
    assert_eq!(
        store.commit_playback_event(&ev, 2_500).unwrap(),
        EventOutcome::Applied
    );
    store
        .renew_play_session(member, "dev_maint", sid, resolved, 2_600)
        .unwrap();
    store
        .issue_play_session(
            member,
            "dev_maint_2",
            file.file_id,
            media_id,
            UnitKey::new(1, 1),
            &file.version,
            2_700,
        )
        .unwrap();
    store
        .mutate_playback(
            media_id,
            &PlaybackMutation::marks(member, 1, 1, Some(true), None),
            2_800,
        )
        .unwrap();
}

#[test]
fn maintenance_writer_on_app_does_not_block_verified_actor_library_writes_and_serializes_app_mutations()
 {
    let (_tmp, f) = seed();
    let store = Arc::new(f.store);
    let member = seed_active_member(&store, &f.library_id, "maint_actor");
    store.set_user_token(member, "tok-maint").unwrap();
    store
        .bind_playback_credential(member, "dev_maint", "tok-maint")
        .unwrap();
    let (sid, generation) =
        issue_and_prime_progress(&store, member, "dev_maint", f.media_id, &f.file);
    let item = TypedItemId::Unit {
        media: f.media_id,
        key: UnitKey::new(1, 1),
    };
    let resolved = store.resolve_playback(member, &item, None, None).unwrap();

    let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
    std::thread::scope(|scope| {
        let store_bg = Arc::clone(&store);
        let release_guard = SignalOnDrop(Some(release_tx.clone()));
        let handle = scope.spawn(move || {
            let _entered_guard = SignalOnDrop(Some(entered_tx.clone()));
            store_bg
                .with_maintenance_writer(Db::App, |_conn| {
                    let _ = entered_tx.try_send(());
                    release_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("maintenance release signal timed out");
                    Ok(())
                })
                .unwrap();
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker did not enter maintenance writer");

        assert_verified_library_writes_succeed(
            &store, member, f.media_id, &f.file, &sid, generation, &resolved,
        );
        assert!(matches!(
            store
                .writers()
                .with_writer_timeout(Duration::from_millis(50), Db::App, |_| Ok(())),
            Err(StoreError::WriteBusy)
        ));

        let _ = release_tx.try_send(());
        drop(release_guard);
        handle.join().unwrap();
    });

    store.revoke_device(member, "dev_maint").unwrap();
    let post_revoke = make_event(
        member,
        "dev_maint",
        &sid,
        generation,
        3,
        f.media_id,
        &f.file,
        9_000,
        PlaybackEventType::Progress,
    );
    assert_eq!(
        store.commit_playback_event(&post_revoke, 3_000).unwrap(),
        EventOutcome::IgnoredStale
    );
    assert!(matches!(
        store.issue_play_session(
            member,
            "dev_maint",
            f.file.file_id,
            f.media_id,
            UnitKey::new(1, 1),
            &f.file.version,
            3_100,
        ),
        Err(StoreError::Protected(_))
    ));
    store
        .save_user(&User {
            id: member,
            login: "maint_actor".into(),
            enabled: false,
            role: UserRole::Member,
        })
        .unwrap();
    assert!(matches!(
        store.mutate_playback(
            f.media_id,
            &PlaybackMutation::marks(member, 1, 1, Some(false), None),
            3_200,
        ),
        Err(StoreError::Protected(_))
    ));
}
