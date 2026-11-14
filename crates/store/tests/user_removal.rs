use domain::{MediaKind, User, UserId, UserRole};
use store::{Db, Store};

fn fixture() -> (tempfile::TempDir, Store, UserId, UserId) {
    let base = std::path::Path::new("/tmp/crawler-storage-fixes");
    std::fs::create_dir_all(base).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let member = UserId::new();
    let other = UserId::new();
    for (id, login, role) in [
        (UserId::new(), "admin", UserRole::Admin),
        (member, "member", UserRole::Member),
        (other, "other", UserRole::Member),
    ] {
        store
            .insert_user(&User {
                id,
                login: login.into(),
                enabled: true,
                role,
            })
            .unwrap();
    }
    (tmp, store, member, other)
}

fn library(store: &Store, tmp: &tempfile::TempDir, name: &str, members: &[UserId]) -> String {
    let root = tmp.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            name,
            &[root.to_str().unwrap()],
            "selected",
            true,
            members,
        )
        .unwrap()
        .id
}

fn stored_credentials(store: &Store, user: UserId) -> (usize, usize) {
    store
        .with_read(Db::App, |conn| {
            let tokens = conn.query_row(
                "SELECT count(*) FROM user_tokens WHERE user_id=?1",
                [user.to_string()],
                |row| row.get(0),
            )?;
            let credentials = conn.query_row(
                "SELECT count(*) FROM playback_device_credentials WHERE user_id=?1",
                [user.to_string()],
                |row| row.get(0),
            )?;
            Ok((tokens, credentials))
        })
        .unwrap()
}

#[test]
fn unknown_user_cannot_receive_a_token() {
    let (_tmp, store, _, _) = fixture();
    let unknown = UserId::new();
    assert!(store.set_user_token(unknown, "unknown-token").is_err());
    assert_eq!(stored_credentials(&store, unknown), (0, 0));
}

#[test]
fn removing_user_drops_tokens_credentials_and_memberships() {
    let (tmp, store, member, other) = fixture();
    let shared = library(&store, &tmp, "shared", &[member, other]);
    let sole = library(&store, &tmp, "sole", &[member]);
    let untouched = library(&store, &tmp, "untouched", &[other]);
    for token in ["first", "second"] {
        store.set_user_token(member, token).unwrap();
        store
            .bind_playback_credential(member, token, token)
            .unwrap();
    }
    store.set_user_token(other, "other-token").unwrap();
    store
        .bind_playback_credential(other, "other-device", "other-token")
        .unwrap();
    assert_eq!(stored_credentials(&store, member), (2, 2));

    assert!(store.delete_user(member).unwrap());
    assert!(store.get_user(member).unwrap().is_none());
    assert_eq!(stored_credentials(&store, member), (0, 0));
    assert_eq!(stored_credentials(&store, other), (1, 1));
    assert_eq!(
        store.get_library(&shared).unwrap().unwrap().member_ids,
        vec![other]
    );
    assert!(
        store
            .get_library(&sole)
            .unwrap()
            .unwrap()
            .member_ids
            .is_empty()
    );
    assert_eq!(
        store.get_library(&untouched).unwrap().unwrap().member_ids,
        vec![other]
    );
    assert!(!store.finish_user_removal(member).unwrap());
    assert_eq!(
        store.get_library(&shared).unwrap().unwrap().member_ids,
        vec![other]
    );
}

#[test]
fn repeated_finish_recovers_membership_cleanup_after_app_user_is_gone() {
    let (tmp, store, member, other) = fixture();
    let shared = library(&store, &tmp, "shared", &[member, other]);
    store
        .with_writer(Db::Library, |conn| {
            conn.execute_batch(
                "CREATE TRIGGER reject_membership_cleanup BEFORE UPDATE OF member_ids_json
                 ON libraries BEGIN SELECT RAISE(ABORT, 'membership write blocked'); END;",
            )?;
            Ok(())
        })
        .unwrap();

    assert!(store.finish_user_removal(member).is_err());
    assert!(store.get_user(member).unwrap().is_none());
    assert_eq!(
        store.get_library(&shared).unwrap().unwrap().member_ids,
        vec![member, other]
    );
    store
        .with_writer(Db::Library, |conn| {
            conn.execute_batch("DROP TRIGGER reject_membership_cleanup")?;
            Ok(())
        })
        .unwrap();

    assert!(!store.finish_user_removal(member).unwrap());
    assert_eq!(
        store.get_library(&shared).unwrap().unwrap().member_ids,
        vec![other]
    );
    assert!(!store.finish_user_removal(member).unwrap());
}

#[test]
fn rotating_token_drops_its_device_credential() {
    let (_tmp, store, member, _) = fixture();
    // Observe the token as an ordinary credential before it becomes the CLI token.
    store.set_user_token(member, "old-cli").unwrap();
    store
        .bind_playback_credential(member, "device", "old-cli")
        .unwrap();
    store
        .put_setting("auth.cli_token.current", "old-cli")
        .unwrap();
    assert_eq!(stored_credentials(&store, member), (1, 1));

    store.register_current_cli_token(member, "new-cli").unwrap();
    assert_eq!(stored_credentials(&store, member), (1, 0));
    assert!(store.user_id_by_token("old-cli").unwrap().is_none());
    assert_eq!(store.user_id_by_token("new-cli").unwrap(), Some(member));
}
