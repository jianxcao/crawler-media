use domain::{
    Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId,
};
use store::Store;
fn fixture() -> (tempfile::TempDir, Store, Subscribe) {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(temp.path()).unwrap();
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Movie,
            title: "retention".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let subscribe = candidate(media);
    (temp, store, subscribe)
}

fn candidate(media: MediaId) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}

fn prepared(store: &Store, subscribe: &Subscribe) -> (Media, Media) {
    let previous = store.get_media(subscribe.media_id).unwrap().unwrap();
    let mut expected = previous.clone();
    expected.title = "ensured".into();
    store.update_media(&expected).unwrap();
    (previous, expected)
}

fn new_ensured_media(store: &Store, template: MediaId) -> Media {
    let mut incoming = store.get_media(template).unwrap().unwrap();
    incoming.id = MediaId::new();
    incoming.tmdb_id = Some(incoming.id.to_string());
    let (media, previous) = store.ensure_media_with_previous(incoming).unwrap();
    assert!(previous.is_none());
    media
}

#[test]
fn rollback_before_retention_purges_new_unreferenced_media() {
    let (_tmp, store, template) = fixture();
    let media = new_ensured_media(&store, template.media_id);
    let subscribe = candidate(media.id);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &media, None)
        .unwrap();

    store.rollback_subscribe_creation(&op).unwrap();
    assert!(store.get_subscribe(subscribe.id).unwrap().is_none());
    assert!(store.get_media(media.id).unwrap().is_none());
    assert!(
        store
            .get_media_by_alias("tmdb", media.tmdb_id.as_deref().unwrap())
            .unwrap()
            .is_none()
    );
    store.recover_subscribe_references(64).unwrap();
    assert!(store.get_media(media.id).unwrap().is_none());
}

#[test]
fn recovery_before_retention_purges_new_unreferenced_media() {
    let (tmp, store, template) = fixture();
    let media = new_ensured_media(&store, template.media_id);
    let subscribe = candidate(media.id);
    store
        .begin_compensatable_subscribe_creation(&subscribe, &media, None)
        .unwrap();
    drop(store);

    let store = Store::open(tmp.path()).unwrap();
    store.recover_subscribe_references(64).unwrap();
    assert!(store.get_subscribe(subscribe.id).unwrap().is_none());
    assert!(store.get_media(media.id).unwrap().is_none());
    assert!(
        store
            .get_media_by_alias("tmdb", media.tmdb_id.as_deref().unwrap())
            .unwrap()
            .is_none()
    );
    store.recover_subscribe_references(64).unwrap();
    assert!(store.get_media(media.id).unwrap().is_none());
}

#[test]
fn rollback_before_retention_preserves_existing_and_referenced_media() {
    let (_tmp, store, template) = fixture();
    let (previous, expected) = prepared(&store, &template);
    let op = store
        .begin_compensatable_subscribe_creation(&template, &expected, Some(&previous))
        .unwrap();
    store.rollback_subscribe_creation(&op).unwrap();
    assert_eq!(store.get_media(previous.id).unwrap(), Some(previous));

    let media = new_ensured_media(&store, template.media_id);
    let subscribe = candidate(media.id);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &media, None)
        .unwrap();
    let collection = store.create_collection(subscribe.user_id, "Keep").unwrap();
    store
        .add_collection_item(&collection, &media.id.to_string())
        .unwrap();
    store.rollback_subscribe_creation(&op).unwrap();
    assert_eq!(store.get_media(media.id).unwrap(), Some(media));
}

#[test]
fn rollback_before_retention_preserves_concurrently_changed_new_media() {
    let (_tmp, store, template) = fixture();
    let media = new_ensured_media(&store, template.media_id);
    let subscribe = candidate(media.id);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &media, None)
        .unwrap();
    store
        .add_media_alias(
            media.id,
            &domain::MediaAlias {
                source: "tmdb".into(),
                kind: media.kind,
                external_id: "concurrent-before-retain".into(),
            },
        )
        .unwrap();
    store.rollback_subscribe_creation(&op).unwrap();
    assert_eq!(store.get_media(media.id).unwrap(), Some(media));
    assert!(
        store
            .get_media_by_alias("tmdb", "concurrent-before-retain")
            .unwrap()
            .is_some()
    );
}
#[test]
fn failed_creation_restores_preexisting_media_and_preserves_concurrent_identity() {
    let (_tmp, store, subscribe) = fixture();
    let (previous, expected) = prepared(&store, &subscribe);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store
        .commit_subscribe_creation(&op, &subscribe, true)
        .unwrap();
    let mut concurrent = expected.clone();
    concurrent.title = "concurrent".into();
    store.update_media(&concurrent).unwrap();
    store.rollback_subscribe_creation(&op).unwrap();
    assert_eq!(store.get_media(expected.id).unwrap(), Some(concurrent));
}
#[test]
fn compensation_reopen_after_library_close_does_not_restore_twice() {
    let (tmp, store, subscribe) = fixture();
    let (previous, expected) = prepared(&store, &subscribe);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store
        .commit_subscribe_creation(&op, &subscribe, true)
        .unwrap();
    let conn = rusqlite::Connection::open(tmp.path().join("subscribe.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_ack BEFORE UPDATE OF state ON subscribe_reference_ops WHEN NEW.state='done' BEGIN SELECT RAISE(FAIL,'ack blocked'); END;").unwrap();
    let _ = store.rollback_subscribe_creation(&op);
    assert_eq!(store.get_media(expected.id).unwrap(), Some(previous));
    store.update_media(&expected).unwrap();
    conn.execute_batch("DROP TRIGGER fail_ack").unwrap();
    drop(conn);
    drop(store);
    let reopened = Store::open(tmp.path()).unwrap();
    reopened.recover_subscribe_references(64).unwrap();
    assert_eq!(reopened.get_media(expected.id).unwrap(), Some(expected));
}

#[test]
fn recovery_compensates_retained_creation_but_user_delete_after_success_purges() {
    let (tmp, store, subscribe) = fixture();
    let (previous, expected) = prepared(&store, &subscribe);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    store.recover_subscribe_references(64).unwrap();
    assert_eq!(
        store.get_media(expected.id).unwrap(),
        Some(previous.clone())
    );
    assert!(store.get_subscribe(subscribe.id).unwrap().is_none());
    let newer = store
        .begin_compensatable_subscribe_creation(&subscribe, &previous, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&newer).unwrap();
    store
        .commit_subscribe_creation(&newer, &subscribe, true)
        .unwrap();
    store.finalize_subscribe_creation(&newer).unwrap();
    store.rollback_subscribe_creation(&op).unwrap();
    assert!(store.get_subscribe(subscribe.id).unwrap().is_some());
    store.delete_subscribe(subscribe.id).unwrap();
    assert!(store.get_media(expected.id).unwrap().is_none());
}

mod query_model_support;

#[test]
fn integrity_owner_defers_compensation_without_releasing_its_retention() {
    let (tmp, store, library, ids) = query_model_support::setup_count(1);
    let subscribe = candidate(ids[0].parse().unwrap());
    let previous = store.get_media(subscribe.media_id).unwrap().unwrap();
    let expected = previous.clone();
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store
        .commit_subscribe_creation(&op, &subscribe, true)
        .unwrap();
    let peer = Store::open(tmp.path()).unwrap();
    peer.begin_library_integrity(library, true, "compensation-owner", 1)
        .unwrap();
    assert!(matches!(
        store.rollback_subscribe_creation(&op),
        Err(store::StoreError::Protected(_))
    ));
    assert_eq!(
        store.get_media(expected.id).unwrap(),
        Some(expected.clone())
    );
    while peer
        .advance_library_operation("compensation-owner", 2, 100)
        .unwrap()
        .state
        == "applying"
    {}
    assert!(
        !store
            .undo_ensured_media_if_unreferenced(expected.id, &expected)
            .unwrap()
    );
    store.recover_subscribe_references(64).unwrap();
    assert_eq!(store.get_media(expected.id).unwrap(), Some(previous));
}

#[test]
fn later_release_chunk_preserves_identity_changed_after_first_close() {
    for nonrefresh in [true, false] {
        let (tmp, store, mut subscribe) = fixture();
        let mut expected = store.get_media(subscribe.media_id).unwrap().unwrap();
        expected.id = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse().unwrap();
        expected.tmdb_id = Some("original-primary".into());
        store.insert_media(&expected).unwrap();
        subscribe.media_id = expected.id;
        let op = store
            .begin_compensatable_subscribe_creation(&subscribe, &expected, None)
            .unwrap();
        store.retain_subscribe_creation(&op).unwrap();
        store
            .commit_subscribe_creation(&op, &subscribe, true)
            .unwrap();
        let generation = store
            .subscribe_job_sync(subscribe.id)
            .unwrap()
            .unwrap()
            .generation;
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        for n in 0..64 {
            let mut extra = expected.clone();
            extra.id = format!("00000000-0000-0000-0000-{n:012}").parse().unwrap();
            extra.tmdb_id = None;
            store.insert_media(&extra).unwrap();
            conn.execute(
                "INSERT INTO subscribe_retention VALUES(?1,?2,?3)",
                rusqlite::params![subscribe.id.to_string(), generation, extra.id.to_string()],
            )
            .unwrap();
        }
        store.rollback_subscribe_creation(&op).unwrap();
        assert_eq!(
            store.get_media(expected.id).unwrap(),
            Some(expected.clone())
        );
        let mut concurrent = expected.clone();
        if nonrefresh {
            store
                .add_media_alias(
                    expected.id,
                    &domain::MediaAlias {
                        source: "tmdb".into(),
                        kind: expected.kind,
                        external_id: "concurrent-extra".into(),
                    },
                )
                .unwrap();
        } else {
            concurrent.title = "concurrent after first close".into();
            store.update_media(&concurrent).unwrap();
        }
        store.recover_subscribe_references(64).unwrap();
        assert_eq!(store.get_media(expected.id).unwrap(), Some(concurrent));
        if nonrefresh {
            assert!(
                store
                    .get_media_by_alias("tmdb", "concurrent-extra")
                    .unwrap()
                    .is_some()
            );
        }
    }
}

#[test]
fn compensation_preserves_prior_and_concurrent_nonrefresh_aliases() {
    for prior_douban in [None, Some("prior-douban")] {
        let (_tmp, store, subscribe) = fixture();
        let mut previous = store.get_media(subscribe.media_id).unwrap().unwrap();
        previous.tmdb_id = Some("same-tmdb".into());
        previous.douban_id = prior_douban.map(str::to_string);
        store.update_media(&previous).unwrap();
        let mut incoming = previous.clone();
        incoming.id = MediaId::new();
        incoming.douban_id = Some("incoming-douban".into());
        let (expected, original) = store.ensure_media_with_previous(incoming).unwrap();
        assert_eq!(original, Some(previous.clone()));
        let op = store
            .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
            .unwrap();
        store.retain_subscribe_creation(&op).unwrap();
        store
            .commit_subscribe_creation(&op, &subscribe, true)
            .unwrap();
        store
            .add_media_alias(
                expected.id,
                &domain::MediaAlias {
                    source: "douban".into(),
                    kind: expected.kind,
                    external_id: "concurrent-nonrefresh".into(),
                },
            )
            .unwrap();
        store.rollback_subscribe_creation(&op).unwrap();
        assert_eq!(
            store.get_media(expected.id).unwrap(),
            Some(previous.clone())
        );
        assert!(
            store
                .get_media_by_alias("douban", "incoming-douban")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_media_by_alias("douban", "concurrent-nonrefresh")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .media_refresh_alias(expected.id, "douban")
                .unwrap()
                .map(|a| a.external_id),
            previous.douban_id
        );
    }
}

#[test]
fn compensation_preserves_a_concurrent_primary_provider_change() {
    let (_tmp, store, subscribe) = fixture();
    let previous = store.get_media(subscribe.media_id).unwrap().unwrap();
    let mut expected = previous.clone();
    expected.douban_id = Some("introduced".into());
    store.update_media(&expected).unwrap();
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store
        .commit_subscribe_creation(&op, &subscribe, true)
        .unwrap();
    store
        .add_media_alias(
            expected.id,
            &domain::MediaAlias {
                source: "tvdb".into(),
                kind: expected.kind,
                external_id: "concurrent-primary".into(),
            },
        )
        .unwrap();
    let concurrent = store.get_media(expected.id).unwrap();
    store.rollback_subscribe_creation(&op).unwrap();
    assert_eq!(store.get_media(expected.id).unwrap(), concurrent);
    assert!(
        store
            .get_media_by_alias("douban", "introduced")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_media_by_alias("tvdb", "concurrent-primary")
            .unwrap()
            .is_some()
    );
}

#[test]
fn newly_created_media_with_concurrent_nonrefresh_alias_is_not_purged() {
    let (_tmp, store, subscribe) = fixture();
    let mut expected = store.get_media(subscribe.media_id).unwrap().unwrap();
    expected.tmdb_id = Some("original-primary".into());
    store.update_media(&expected).unwrap();
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, None)
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store
        .commit_subscribe_creation(&op, &subscribe, true)
        .unwrap();
    store
        .add_media_alias(
            expected.id,
            &domain::MediaAlias {
                source: "tmdb".into(),
                kind: expected.kind,
                external_id: "concurrent-extra".into(),
            },
        )
        .unwrap();
    assert_eq!(
        store.get_media(expected.id).unwrap(),
        Some(expected.clone())
    );
    store.rollback_subscribe_creation(&op).unwrap();
    assert_eq!(store.get_media(expected.id).unwrap(), Some(expected));
    assert!(
        store
            .get_media_by_alias("tmdb", "concurrent-extra")
            .unwrap()
            .is_some()
    );
}

#[test]
fn integrity_repair_of_orphan_projection_blocks_actual_identity_restore() {
    let (tmp, store, library, ids) = query_model_support::setup_count(1);
    let subscribe = candidate(ids[0].parse().unwrap());
    let (previous, expected) = prepared(&store, &subscribe);
    let op = store
        .begin_compensatable_subscribe_creation(&subscribe, &expected, Some(&previous))
        .unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    store
        .commit_subscribe_creation(&op, &subscribe, true)
        .unwrap();
    // Fault fixture: Integrity owns repair of a surviving derived item whose
    // raw file fact was lost. This is not a normal file-deletion flow.
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", "OFF").unwrap();
    db.execute(
        "DELETE FROM library_files WHERE media_id=?1",
        [expected.id.to_string()],
    )
    .unwrap();
    assert!(
        store
            .get_library_item(library, expected.id)
            .unwrap()
            .is_some()
    );
    let peer = Store::open(tmp.path()).unwrap();
    peer.begin_library_integrity(library, true, "orphan-projection-owner", 1)
        .unwrap();
    assert!(matches!(
        store.rollback_subscribe_creation(&op),
        Err(store::StoreError::Protected(_))
    ));
    assert_eq!(
        store.get_media(expected.id).unwrap(),
        Some(expected.clone())
    );
    while peer
        .advance_library_operation("orphan-projection-owner", 2, 100)
        .unwrap()
        .state
        == "applying"
    {}
    assert!(
        !store
            .undo_ensured_media_if_unreferenced(expected.id, &expected)
            .unwrap()
    );
    store.recover_subscribe_references(64).unwrap();
    assert_eq!(store.get_media(expected.id).unwrap(), Some(previous));
}
