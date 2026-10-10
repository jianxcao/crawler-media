use std::str::FromStr;

use domain::{MediaId, UserId};
use rusqlite::{Connection, params};
use store::Store;

#[test]
fn old_play_logs_gain_nullable_identity_without_inventing_devices() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let user = UserId::new();
    let media = MediaId::new();
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute_batch("ALTER TABLE playback_logs DROP COLUMN device_id; PRAGMA user_version=2;")
        .unwrap();
    db.execute(
        "INSERT INTO playback_logs (id,user_id,media_id,client,device_name,play_method,started_at,ended_at)
         VALUES ('old',?1,?2,'Infuse','Same Name','DirectPlay',1,2)",
        params![user.to_string(), media.to_string()],
    ).unwrap();
    for _ in 0..2 {
        let reopened = Store::open(tmp.path()).unwrap();
        let logs = reopened
            .list_logs(10, None, None, None, 10, Some(user))
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].device_id, None);
        assert!(!logs[0].revocable());
        assert!(!reopened.device_is_revocable(user, "jf-Same Name").unwrap());
    }
}

#[test]
fn failed_movie_mark_does_not_hide_legacy_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let media = MediaId::new();
    store.insert_media(&domain::Media {
        id: media, kind: domain::MediaKind::Movie, title: "Legacy".into(), year: None,
        original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
    }).unwrap();
    store.upsert_unit(user, media, 0, 0, 45000, Some(false), Some(true), Some(90000), None, None, false, 10).unwrap();
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER review_mark_update_block BEFORE UPDATE ON playback_units BEGIN SELECT RAISE(ABORT,'review mark blocked'); END;").unwrap();
    drop(db);
    let blocked = Store::open(tmp.path()).unwrap();
    assert!(blocked.set_unit_marks(user, media, -1, -1, Some(true), None, 20).is_err());
    drop(blocked);
    let reopened = Store::open(tmp.path()).unwrap();
    let legacy = reopened.unit_state(user, media, 0, 0).unwrap().unwrap();
    assert_eq!(legacy.position_ms, 45000);
    assert!(legacy.favorite);
    assert!(reopened.unit_state(user, media, -1, -1).unwrap().is_none());
}

fn old_library_gains_nullable_release_quality_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("ALTER TABLE ledger DROP COLUMN release_quality; PRAGMA user_version=5;")
        .unwrap();
    let media = MediaId::new();
    db.execute("INSERT INTO ledger (id,media_id,path,quality_source,confidence) VALUES ('old',?1,'old.mkv','probe','high')",[media.to_string()]).unwrap();
    for _ in 0..2 {
        let reopened = Store::open(tmp.path()).unwrap();
        assert_eq!(
            reopened
                .schema_versions()
                .into_iter()
                .find(|(name, _)| *name == "library")
                .unwrap()
                .1,
            6
        );
        let quality: Option<String> = db
            .query_row(
                "SELECT release_quality FROM ledger WHERE id='old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quality, None);
    }
}

#[test]
fn clear_viewing_facts_preserves_preferences_and_other_users_and_time_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let other = UserId::new();
    let media = MediaId::new();
    let outside = MediaId::new();
    for (who, what, at) in [(user, media, 100), (other, media, 100), (user, outside, 10)] {
        store
            .upsert_unit(
                who,
                what,
                1,
                1,
                1000,
                Some(true),
                Some(true),
                Some(120000),
                Some("embedded:1"),
                Some("off"),
                true,
                at,
            )
            .unwrap();
    }
    assert_eq!(store.clear_units(user, None, Some(50)).unwrap(), 1);
    let cleared = store.unit_state(user, media, 1, 1).unwrap().unwrap();
    assert_eq!(
        (cleared.position_ms, cleared.played, cleared.play_count),
        (0, false, 0)
    );
    assert_eq!(
        (
            cleared.favorite,
            cleared.audio_track.as_deref(),
            cleared.subtitle_track.as_deref()
        ),
        (true, Some("embedded:1"), Some("off"))
    );
    assert_eq!(
        store
            .unit_state(other, media, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store
            .unit_state(user, outside, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store.user_active_media_ids(user).unwrap(),
        vec![(outside, 10)]
    );
}

#[test]
fn critical_unit_mark_cascade_failure_is_propagated_and_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    for (season, episode) in [(-1, -1), (1, 1)] {
        store
            .upsert_unit(
                user,
                media,
                season,
                episode,
                0,
                None,
                Some(true),
                None,
                None,
                None,
                false,
                1,
            )
            .unwrap();
    }
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER block_mark BEFORE UPDATE ON playback_units WHEN OLD.season=1 BEGIN SELECT RAISE(ABORT,'mark blocked'); END;").unwrap();
    assert!(
        store
            .set_unit_marks(user, media, -1, -1, None, Some(false), 2)
            .is_err()
    );
    for (season, episode) in [(-1, -1), (1, 1)] {
        assert!(
            store
                .unit_state(user, media, season, episode)
                .unwrap()
                .unwrap()
                .favorite
        );
    }
}
