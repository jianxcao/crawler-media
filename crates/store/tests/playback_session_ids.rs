use domain::{LedgerId, UserId};
use store::{PlaySessionBinding, PlaySessionIdState, SessionRow, Store};

#[test]
fn issued_id_is_bound_to_user_device_and_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user_a = UserId::new();
    let user_b = UserId::new();
    let ledger_id = LedgerId::new();

    let id = store
        .issue_play_session(user_a, "device_1", ledger_id, 1000)
        .unwrap();

    // Matching user, device, and id returns binding
    let binding = store
        .play_session_binding(user_a, "device_1", &id)
        .unwrap()
        .expect("binding must exist");
    assert_eq!(binding.ledger_id, ledger_id);
    assert_eq!(binding.issued_at, 1000);
    assert_eq!(binding.generation, 1);
    assert_eq!(binding.state, PlaySessionIdState::Pending);

    // Mismatched user returns None
    assert!(
        store
            .play_session_binding(user_b, "device_1", &id)
            .unwrap()
            .is_none()
    );

    // Mismatched device returns None
    assert!(
        store
            .play_session_binding(user_a, "device_2", &id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn closed_or_older_pending_cannot_reactivate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user = UserId::new();
    let ledger_a = LedgerId::new();
    let ledger_b = LedgerId::new();

    // Issue A at t = 1000, generation = 1
    let id_a = store
        .issue_play_session(user, "dev1", ledger_a, 1000)
        .unwrap();

    // Issue B at t = 1000 (same second!), generation = 2
    let id_b = store
        .issue_play_session(user, "dev1", ledger_b, 1000)
        .unwrap();

    // Activate B: this should activate B and close older pending A (generation 1 < 2)
    let ok_b = store.activate_play_session(user, "dev1", &id_b).unwrap();
    assert!(ok_b, "activating B must succeed");

    let binding_b = store
        .play_session_binding(user, "dev1", &id_b)
        .unwrap()
        .unwrap();
    assert_eq!(binding_b.state, PlaySessionIdState::Active);

    // Old pending A should now be Closed
    let binding_a = store
        .play_session_binding(user, "dev1", &id_a)
        .unwrap()
        .unwrap();
    assert_eq!(binding_a.state, PlaySessionIdState::Closed);

    // Trying to activate A now MUST FAIL
    let ok_a = store.activate_play_session(user, "dev1", &id_a).unwrap();
    assert!(!ok_a, "activating closed A must fail");
}

#[test]
fn repeated_activation_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user = UserId::new();
    let ledger = LedgerId::new();

    let id = store
        .issue_play_session(user, "dev1", ledger, 1000)
        .unwrap();
    assert!(store.activate_play_session(user, "dev1", &id).unwrap());
    // Second activation of same active session returns true (idempotent)
    assert!(store.activate_play_session(user, "dev1", &id).unwrap());

    // Explicit close
    store.close_play_session_id(user, "dev1", &id).unwrap();
    let binding = store
        .play_session_binding(user, "dev1", &id)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Closed);

    // Activation after close returns false
    assert!(!store.activate_play_session(user, "dev1", &id).unwrap());
}

#[test]
fn expiry_keeps_active_and_is_bounded_500() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user = UserId::new();
    let ledger = LedgerId::new();

    // Issue 501 sessions at t = 100 (older than 30 days ago from now)
    let t_old = 100;
    for _ in 0..500 {
        let id = store
            .issue_play_session(user, "dev1", ledger, t_old)
            .unwrap();
        store.close_play_session_id(user, "dev1", &id).unwrap();
    }
    let id_501 = store
        .issue_play_session(user, "dev1", ledger, t_old)
        .unwrap();
    store.close_play_session_id(user, "dev1", &id_501).unwrap();

    // Plus 1 active session at t_old
    let id_active = store
        .issue_play_session(user, "dev1", ledger, t_old)
        .unwrap();
    store.activate_play_session(user, "dev1", &id_active).unwrap();

    // Now issue a new session at t = 100_000_000 (triggering clean_expired bounded at 500)
    let now = 100_000_000;
    let _id_new = store
        .issue_play_session(user, "dev1", ledger, now)
        .unwrap();

    // Active session MUST NOT be deleted
    let active_binding = store
        .play_session_binding(user, "dev1", &id_active)
        .unwrap();
    assert!(active_binding.is_some(), "active session must not be deleted");

    // Out of 501 closed old sessions, 500 were cleaned, leaving 1 closed + 1 active + 1 new pending = 3 rows
    let sub_conn =
        rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    let count: i64 = sub_conn
        .query_row("SELECT COUNT(*) FROM playback_session_ids", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 3, "expected 1 remaining old closed + 1 active + 1 new pending");
}
