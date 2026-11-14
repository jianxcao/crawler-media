use domain::{User, UserId, UserRole};
use store::Store;

fn fixture() -> (tempfile::TempDir, Store, UserId) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    store
        .insert_user(&User {
            id: user,
            login: "playback".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    (tmp, store, user)
}

#[test]
fn revoke_only_observed_credentials_and_persist_bindings() {
    let (tmp, store, user) = fixture();
    for token in ["bedroom-token", "living-token", "web-token"] {
        store.set_user_token(user, token).unwrap();
    }
    store
        .bind_playback_credential(user, "bedroom", "bedroom-token")
        .unwrap();
    store
        .bind_playback_credential(user, "living", "living-token")
        .unwrap();
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    assert!(store.device_is_revocable(user, "bedroom").unwrap());
    store.revoke_device(user, "bedroom").unwrap();
    assert!(store.user_id_by_token("bedroom-token").unwrap().is_none());
    assert_eq!(store.user_id_by_token("living-token").unwrap(), Some(user));
    assert_eq!(store.user_id_by_token("web-token").unwrap(), Some(user));
    assert!(store.is_device_revoked(user, "bedroom").unwrap());
}

#[test]
fn shared_token_is_revoked_for_every_device_using_it() {
    let (_tmp, store, user) = fixture();
    store.set_user_token(user, "shared").unwrap();
    for device in ["bedroom", "living"] {
        store
            .bind_playback_credential(user, device, "shared")
            .unwrap();
    }
    store.revoke_device(user, "bedroom").unwrap();
    assert!(store.user_id_by_token("shared").unwrap().is_none());
    assert!(!store.device_is_revocable(user, "living").unwrap());
}

#[test]
fn unbound_and_cli_only_devices_cannot_be_revoked() {
    let (_tmp, store, user) = fixture();
    store.set_user_token(user, "cli").unwrap();
    store.put_setting("auth.cli_token.current", "cli").unwrap();
    store
        .bind_playback_credential(user, "cli-device", "cli")
        .unwrap();
    for device in ["old-device", "cli-device"] {
        assert!(!store.device_is_revocable(user, device).unwrap());
        assert!(store.revoke_device(user, device).is_err());
        assert!(!store.is_device_revoked(user, device).unwrap());
    }
    assert_eq!(store.user_id_by_token("cli").unwrap(), Some(user));
}

#[test]
fn token_delete_and_blacklist_are_atomic() {
    let (tmp, store, user) = fixture();
    store.set_user_token(user, "ordinary").unwrap();
    store
        .bind_playback_credential(user, "bedroom", "ordinary")
        .unwrap();
    let conn = rusqlite::Connection::open(tmp.path().join("app.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_blacklist BEFORE INSERT ON settings WHEN NEW.key='playback.revoked_devices' BEGIN SELECT RAISE(ABORT,'blocked'); END;").unwrap();
    assert!(store.revoke_device(user, "bedroom").is_err());
    assert_eq!(store.user_id_by_token("ordinary").unwrap(), Some(user));
    assert!(!store.is_device_revoked(user, "bedroom").unwrap());
}

#[test]
fn token_cascade_work_is_bounded_and_preserves_unrelated_credentials() {
    let (tmp, store, user) = fixture();
    let other = UserId::new();
    store
        .insert_user(&User {
            id: other,
            login: "unrelated".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("app.db")).unwrap();
    let revoke = |token: &str| {
        store.set_user_token(user, token).unwrap();
        for device in ["shared-a", "shared-b"] {
            store.bind_playback_credential(user, device, token).unwrap();
        }
        let (_, stats) = store.with_stats(store::stats::StatsScope::App, || {
            store.revoke_device(user, "shared-a").unwrap()
        });
        assert!(!store.device_is_revocable(user, "shared-b").unwrap());
        let bindings: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM playback_device_credentials WHERE token=?1",
                [token],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bindings, 0);
        assert!(stats.vm_steps > 0, "{stats:?}");
        stats
    };
    let small = revoke("small");
    store.set_user_token(other, "unrelated").unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for n in 0..2000 {
        tx.execute("INSERT INTO playback_device_credentials(user_id,device_id,token) VALUES(?1,?2,'unrelated')",rusqlite::params![other.to_string(),format!("device-{n}")]).unwrap();
    }
    tx.commit().unwrap();
    store
        .bind_playback_credential(user, "wrong-owner", "unrelated")
        .unwrap();
    assert!(!store.device_is_revocable(user, "wrong-owner").unwrap());
    let large = revoke("large");
    assert_eq!(store.user_id_by_token("unrelated").unwrap(), Some(other));
    assert!(store.device_is_revocable(other, "device-1999").unwrap());
    assert!(
        large.vm_steps <= small.vm_steps + 200,
        "{small:?} -> {large:?}"
    );
    assert!(
        large.fullscan_steps <= small.fullscan_steps + 10,
        "{small:?} -> {large:?}"
    );
}
