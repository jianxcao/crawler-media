use std::collections::BTreeMap;

use domain::{MediaId, MediaKind, UserId};
use rusqlite::Connection;
use store::Store;
use store::playback::{MediaMergeRequest, PlaybackMutation};

mod merge_support;
mod playback_event_support;

const USER: &str = "00000000-0000-0000-0002-000000000001";
const SURVIVOR: &str = "00000000-0000-0000-0000-0000000000a1";
const LOSER: &str = "00000000-0000-0000-0000-0000000000a2";

fn begin(store: &Store, survivor: MediaId, loser: MediaId) -> String {
    store
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000a1".into(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: BTreeMap::from([("tmdb".into(), survivor.to_string())]),
            },
            1_000,
        )
        .unwrap()
}

fn seed(store: &Store) -> (MediaId, MediaId, UserId) {
    for (id, title) in [(SURVIVOR, "Survivor"), (LOSER, "Loser")] {
        merge_support::create_media(store, id, title, MediaKind::Movie);
    }
    (
        SURVIVOR.parse().unwrap(),
        LOSER.parse().unwrap(),
        USER.parse().unwrap(),
    )
}

#[test]
fn frozen_and_retired_marks_are_rejected_without_changing_other_users_or_survivor() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (survivor, loser, user) = seed(&store);
    let other = UserId::new();
    let marks = PlaybackMutation::marks(user, -1, -1, Some(true), Some(true));
    store.mutate_playback(survivor, &marks, 500).unwrap();
    store.mutate_playback(loser, &marks, 600).unwrap();
    let before = store.unit_state(user, survivor, -1, -1).unwrap();
    let operation = begin(&store, survivor, loser);
    for target in [survivor, loser] {
        let error = store.mutate_playback(target, &marks, 1_100).unwrap_err();
        assert!(matches!(error, store::StoreError::Protected(_)), "{error}");
        assert!(
            store
                .mutate_playback(
                    target,
                    &PlaybackMutation::marks(other, -1, -1, None, Some(true)),
                    1_100
                )
                .is_err()
        );
    }
    assert_eq!(store.unit_state(user, survivor, -1, -1).unwrap(), before);
    assert!(store.unit_rows(other, survivor).unwrap().is_empty());
    assert!(store.unit_rows(other, loser).unwrap().is_empty());
    assert_eq!(
        store.advance_media_merge(&operation, 1_200).unwrap().state,
        "succeeded"
    );
    let merged = store.unit_state(user, survivor, -1, -1).unwrap();
    let error = store.mutate_playback(loser, &marks, 1_300).unwrap_err();
    assert!(error.to_string().contains(&survivor.to_string()), "{error}");
    assert_eq!(store.unit_state(user, survivor, -1, -1).unwrap(), merged);
    assert!(store.unit_rows(user, loser).unwrap().is_empty());
    store
        .mutate_playback(
            survivor,
            &PlaybackMutation::marks(user, -1, -1, Some(false), Some(false)),
            1_400,
        )
        .unwrap();
    let unit = store.unit_state(user, survivor, -1, -1).unwrap().unwrap();
    assert!(!unit.played && !unit.favorite);
    drop(store);
    assert!(
        Store::open(tmp.path())
            .unwrap()
            .unit_rows(user, loser)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn interrupted_merge_rejects_late_marks_and_resumes_after_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (survivor, loser, user) = seed(&store);
    let marks = PlaybackMutation::marks(user, -1, -1, Some(true), Some(true));
    store.mutate_playback(loser, &marks, 500).unwrap();
    let expected = store.unit_state(user, loser, -1, -1).unwrap();
    let operation = begin(&store, survivor, loser);
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_retire BEFORE UPDATE OF phase_cursor ON media_operations
        WHEN NEW.phase_cursor='retire' BEGIN SELECT RAISE(ABORT,'injected boundary failure'); END;",
    )
    .unwrap();
    assert!(store.advance_media_merge(&operation, 1_050).is_err());
    assert_eq!(
        store.media_operation_status(&operation).unwrap().phase,
        "subscribes"
    );
    assert!(store.mutate_playback(loser, &marks, 1_100).is_err());
    assert!(store.unit_rows(user, loser).unwrap().is_empty());
    assert_eq!(store.unit_state(user, survivor, -1, -1).unwrap(), expected);
    db.execute_batch("DROP TRIGGER fail_retire").unwrap();
    drop(db);
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    assert_eq!(
        store.advance_media_merge(&operation, 1_200).unwrap().state,
        "succeeded"
    );
    assert_eq!(store.unit_state(user, survivor, -1, -1).unwrap(), expected);
    assert!(store.unit_rows(user, loser).unwrap().is_empty());
}

#[test]
fn frozen_direct_units_tracks_metrics_and_session_issuance_cannot_create_facts() {
    use playback_event_support::*;
    let (_tmp, f) = playback_event_support::seed();
    let survivor = merge_support::create_media(&f.store, SURVIVOR, "Survivor", MediaKind::Tv).id;
    let user: UserId = playback_event_support::USER.parse().unwrap();
    let item = episode(&f, 1);
    let (session, generation) = issue(&f, &item, &f.file);
    let event = event(
        &f,
        item,
        (&session, generation),
        &f.file,
        50_000,
        PlaybackEventType::Playing,
    );
    assert_eq!(
        f.store.commit_playback_event(&event, 1_000_000).unwrap(),
        EventOutcome::Applied
    );
    let live = f.store.get_session(user, "device_web").unwrap().unwrap();
    let before = f.store.unit_state(user, f.media_id, 1, 1).unwrap();
    let summary_before = f
        .store
        .get_user_library_summary(user, f.library_id.parse().unwrap(), f.media_id)
        .unwrap();
    begin(&f.store, survivor, f.media_id);
    assert!(
        f.store
            .upsert_unit(
                user,
                f.media_id,
                1,
                1,
                70_000,
                None,
                Some(true),
                None,
                Some("2"),
                None,
                false,
                1_100
            )
            .is_err()
    );
    assert!(
        f.store
            .upsert_unit_for_source(
                user,
                f.media_id,
                UnitKey::new(1, 1),
                event.source_id,
                70_000,
                None,
                None,
                None,
                Some("2"),
                None,
                false,
                1_100
            )
            .is_err()
    );
    assert!(
        f.store
            .clear_units(user, Some(&[f.media_id]), None)
            .is_err()
    );
    assert!(
        f.store
            .insert_metric(
                user, f.media_id, 0, None, None, 0, 0, 0, None, None, 0, 1_100
            )
            .is_err()
    );
    assert!(
        f.store
            .issue_play_session(
                user,
                "new_device",
                f.file.file_id,
                f.media_id,
                UnitKey::new(1, 1),
                &f.file.version,
                1_100
            )
            .is_err()
    );
    assert!(f.store.upsert_session(&live).is_err());
    assert!(f.store.close_session(user, "device_web", 1_100).is_err());
    assert_eq!(f.store.unit_state(user, f.media_id, 1, 1).unwrap(), before);
    assert_eq!(f.store.get_session(user, "device_web").unwrap(), Some(live));
    assert_eq!(
        f.store
            .get_user_library_summary(user, f.library_id.parse().unwrap(), f.media_id,)
            .unwrap(),
        summary_before
    );
    assert!(
        f.store
            .list_logs(100, None, None, None, 100, Some(user))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn signed_events_for_frozen_media_are_ignored_without_creating_or_closing_facts() {
    use playback_event_support::*;
    let (_tmp, f) = playback_event_support::seed();
    let survivor = merge_support::create_media(&f.store, SURVIVOR, "Survivor", MediaKind::Tv).id;
    let item = episode(&f, 1);
    let (session, generation) = issue(&f, &item, &f.file);
    let mut event = event(
        &f,
        item,
        (&session, generation),
        &f.file,
        50_000,
        PlaybackEventType::Playing,
    );
    let operation = begin(&f.store, survivor, f.media_id);
    for kind in [
        PlaybackEventType::Playing,
        PlaybackEventType::Progress,
        PlaybackEventType::Stopped,
    ] {
        event.event_type = kind;
        assert_eq!(
            f.store.commit_playback_event(&event, 1_100_000).unwrap(),
            EventOutcome::IgnoredStale
        );
        assert!(
            f.store
                .unit_rows(event.user_id, f.media_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            f.store
                .get_session(event.user_id, "device_web")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.store
                .play_session_binding(event.user_id, "device_web", &session)
                .unwrap()
                .unwrap()
                .state,
            PlaySessionIdState::Pending
        );
    }
    assert_eq!(
        f.store
            .advance_media_merge(&operation, 1_200_000)
            .unwrap()
            .state,
        "succeeded"
    );
    assert_eq!(
        f.store.commit_playback_event(&event, 1_300_000).unwrap(),
        EventOutcome::IgnoredStale
    );
    assert!(
        f.store
            .unit_rows(event.user_id, f.media_id)
            .unwrap()
            .is_empty()
    );
}

fn rejected_mark_work(history: usize) -> store::library_query::items::QueryWorkStats {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (survivor, loser, user) = seed(&store);
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for index in 0..history {
        tx.execute(
            "INSERT INTO media_operations(operation_id,kind,state,survivor_id,loser_id,
                plan_json,phase_cursor,created_at_ms,updated_at_ms)
             VALUES (?1,'merge','succeeded',?2,?3,'{}','retire',0,0)",
            rusqlite::params![
                format!("history-{index}"),
                survivor.to_string(),
                loser.to_string()
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    db.execute_batch("ANALYZE").unwrap();
    drop(db);
    begin(&store, survivor, loser);
    let (result, work) = store.with_stats(store::stats::StatsScope::Library, || {
        store.mutate_playback(
            loser,
            &PlaybackMutation::marks(user, -1, -1, None, Some(true)),
            1_100,
        )
    });
    assert!(matches!(result, Err(store::StoreError::Protected(_))));
    assert!(store.unit_rows(user, loser).unwrap().is_empty());
    work
}

#[test]
fn frozen_mark_rejection_does_not_scan_terminal_merge_history() {
    let small = rejected_mark_work(1_000);
    let large = rejected_mark_work(20_000);
    assert!(
        large.vm_steps <= small.vm_steps + 100,
        "{small:?} -> {large:?}"
    );
    assert_eq!(large.fullscan_steps, small.fullscan_steps);
}
