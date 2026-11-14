//! Late reassignment must respect the owning Library's durable publication barrier.

use super::*;
use store::library_operations::LibraryReconfiguration;

fn request(store: &Store, from: MediaId, to: MediaId, ids: &[LedgerId]) -> ReassignFiles {
    ReassignFiles {
        files: ids
            .iter()
            .map(|id| (*id, file(store, *id).version))
            .collect(),
        from,
        to,
        expected_media_revision: revision(store, from),
    }
}

fn seed_progress(store: &Store, user: UserId, media: MediaId, id: LedgerId) {
    store
        .upsert_unit_for_source(
            user,
            media,
            UnitKey::WHOLE,
            source_of(store, id),
            5_000,
            Some(true),
            Some(true),
            Some(120_000),
            None,
            None,
            true,
            10,
        )
        .expect("seed playback facts");
}

#[test]
fn captured_removal_rejects_reassignment_and_remains_cleanable() {
    let fx = fixture();
    let store = &fx.store;
    let from = media(store, MediaKind::Movie, "From");
    let to = media(store, MediaKind::Movie, "To");
    let id = publish(store, &fx.root, from, "captured", None);
    let user = UserId::new();
    seed_progress(store, user, from, id);
    let session = issue_session(store, user, "removal-device", id, 10);
    let before_file = file(store, id);
    let pending = request(store, from, to, &[id]);
    let removal = store
        .begin_library_removal(&before_file.library_id.to_string())
        .expect("freeze and capture owning Library");
    assert!(
        store
            .capture_library_removal_batch(&removal.operation_id)
            .expect("complete ownership snapshot")
    );
    let before_facts = store.playback_units_for_user(user).unwrap();
    let before_sessions = store.sessions_for_device("removal-device").unwrap();

    let error = store
        .reassign_files(&pending, 20)
        .expect_err("captured ownership must never change");

    assert!(matches!(error, StoreError::Protected(_)), "{error:?}");
    assert_eq!(file(store, id), before_file);
    assert_eq!(store.playback_units_for_user(user).unwrap(), before_facts);
    assert_eq!(
        store.sessions_for_device("removal-device").unwrap(),
        before_sessions
    );
    let binding = store
        .play_session_binding(user, "removal-device", &session)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Pending);
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    assert_eq!(
        store
            .clean_library_removal(&removal.operation_id)
            .expect("unchanged snapshot can finish removal")
            .state,
        "succeeded"
    );
    assert!(store.published_files(&[id]).unwrap().is_empty());
    assert!(store.get_media(from).unwrap().is_none());
    assert!(store.playback_units_for_user(user).unwrap().is_empty());
    assert!(fx.root.join("captured.mkv").exists());
}

#[test]
fn mixed_library_reassignment_rejects_entire_batch_when_one_owner_is_frozen() {
    let fx = fixture();
    let store = &fx.store;
    let from = media(store, MediaKind::Movie, "From");
    let to = media(store, MediaKind::Movie, "To");
    let frozen_file = publish(store, &fx.root, from, "frozen", None);
    let ready_root = fx.root.parent().unwrap().join("ready");
    std::fs::create_dir_all(&ready_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Ready",
            &[ready_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let ready_file = publish(store, &ready_root, from, "ready", None);
    let user = UserId::new();
    seed_progress(store, user, from, ready_file);
    let session = issue_session(store, user, "ready-device", ready_file, 10);
    let before_files = store.published_files(&[ready_file, frozen_file]).unwrap();
    // Put the ready Library first: no mutation may precede validation of every owner.
    let pending = request(store, from, to, &[ready_file, frozen_file]);
    store
        .begin_library_removal(&file(store, frozen_file).library_id.to_string())
        .unwrap();
    let before_facts = store.playback_units_for_user(user).unwrap();
    let before_sessions = store.sessions_for_device("ready-device").unwrap();

    let error = store
        .reassign_files(&pending, 20)
        .expect_err("one frozen owner rejects the complete batch");

    assert!(matches!(error, StoreError::Protected(_)), "{error:?}");
    assert_eq!(
        store.published_files(&[ready_file, frozen_file]).unwrap(),
        before_files
    );
    assert_eq!(store.playback_units_for_user(user).unwrap(), before_facts);
    assert_eq!(
        store.sessions_for_device("ready-device").unwrap(),
        before_sessions
    );
    assert_eq!(
        store
            .play_session_binding(user, "ready-device", &session)
            .unwrap()
            .unwrap()
            .state,
        PlaySessionIdState::Pending
    );
}

#[test]
fn reconfiguring_library_rejects_reassignment_without_changing_file_or_playback() {
    let fx = fixture();
    let store = &fx.store;
    let from = media(store, MediaKind::Movie, "From");
    let to = media(store, MediaKind::Movie, "To");
    let id = publish(store, &fx.root, from, "reindexing", None);
    let user = UserId::new();
    seed_progress(store, user, from, id);
    let before_file = file(store, id);
    let before_facts = store.playback_units_for_user(user).unwrap();
    let pending = request(store, from, to, &[id]);
    let library_id = before_file.library_id;
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: uuid::Uuid::new_v4().to_string(),
                library_id,
                expected_revision: store
                    .library_config_revision(&library_id.to_string())
                    .unwrap(),
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            15,
        )
        .expect("freeze Library for locale reconfiguration");
    assert_eq!(
        store
            .library_lifecycle_state(&library_id.to_string())
            .unwrap(),
        "reindexing"
    );

    let error = store
        .reassign_files(&pending, 20)
        .expect_err("reconfiguration owns the Library's file facts");

    assert!(matches!(error, StoreError::Protected(_)), "{error:?}");
    assert_eq!(file(store, id), before_file);
    assert_eq!(store.playback_units_for_user(user).unwrap(), before_facts);
}
