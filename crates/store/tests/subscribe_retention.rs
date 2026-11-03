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
    let subscribe = Subscribe {
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
    };
    (temp, store, subscribe)
}
#[test]
fn deleting_last_subscribe_purges_orphan_but_preserves_another_reference() {
    let (_temp, store, subscribe) = fixture();
    store.insert_subscribe(&subscribe).unwrap();
    let mut other = subscribe.clone();
    other.id = SubscribeId::new();
    store.insert_subscribe(&other).unwrap();
    store.delete_subscribe(subscribe.id).unwrap();
    assert!(store.get_media(subscribe.media_id).unwrap().is_some());
    store.delete_subscribe(other.id).unwrap();
    assert!(
        store.get_media(subscribe.media_id).unwrap().is_none(),
        "last Subscribe release must purge orphan Media"
    );
}
#[test]
fn removing_last_collection_membership_purges_orphan() {
    let (_temp, store, subscribe) = fixture();
    let collection = store
        .create_collection(subscribe.user_id, "retention")
        .unwrap();
    store
        .add_collection_item(&collection, &subscribe.media_id.to_string())
        .unwrap();
    store
        .remove_collection_item(&collection, &subscribe.media_id.to_string())
        .unwrap();
    assert!(
        store.get_media(subscribe.media_id).unwrap().is_none(),
        "last Collection release must purge orphan Media"
    );
}
#[test]
fn creation_cannot_commit_before_retention_and_recovery_closes_delayed_creator() {
    let (temp, store, subscribe) = fixture();
    let creation = store.begin_subscribe_creation(&subscribe).unwrap();
    assert!(
        store
            .commit_subscribe_creation(&creation, &subscribe, false)
            .is_err(),
        "unretained creating intent cannot business commit"
    );
    let peer = Store::open(temp.path()).unwrap();
    peer.recover_subscribe_references(64).unwrap();
    assert!(store.retain_subscribe_creation(&creation).is_err());
    assert!(
        store
            .commit_subscribe_creation(&creation, &subscribe, false)
            .is_err()
    );
    assert!(store.get_subscribe(subscribe.id).unwrap().is_none());
}
#[test]
fn retained_creator_wins_commit_or_abort_cas_and_old_release_preserves_recreation() {
    let (temp, store, subscribe) = fixture();
    let creation = store.begin_subscribe_creation(&subscribe).unwrap();
    store.retain_subscribe_creation(&creation).unwrap();
    store
        .commit_subscribe_creation(&creation, &subscribe, false)
        .unwrap();
    Store::open(temp.path())
        .unwrap()
        .abort_subscribe_creation(&creation)
        .unwrap();
    assert!(store.get_media(subscribe.media_id).unwrap().is_some());
    let collection = store.create_collection(subscribe.user_id, "keep").unwrap();
    store
        .add_collection_item(&collection, &subscribe.media_id.to_string())
        .unwrap();
    store.delete_subscribe(subscribe.id).unwrap();
    store.insert_subscribe(&subscribe).unwrap();
    store.abort_subscribe_creation(&creation).unwrap();
    assert!(store.get_media(subscribe.media_id).unwrap().is_some());
    assert!(store.get_subscribe(subscribe.id).unwrap().is_some());
}
#[test]
fn protected_creation_cannot_be_committed_for_a_different_media_or_id() {
    let (_temp, store, subscribe) = fixture();
    let op = store.begin_subscribe_creation(&subscribe).unwrap();
    store.retain_subscribe_creation(&op).unwrap();
    let mut wrong = subscribe.clone();
    wrong.media_id = MediaId::new();
    assert!(store.commit_subscribe_creation(&op, &wrong, false).is_err());
    wrong = subscribe.clone();
    wrong.id = SubscribeId::new();
    assert!(store.commit_subscribe_creation(&op, &wrong, false).is_err());
    store
        .commit_subscribe_creation(&op, &subscribe, false)
        .unwrap();
    store.abort_subscribe_creation(&op).unwrap();
    assert!(store.get_subscribe(subscribe.id).unwrap().is_some());
    assert!(store.get_media(subscribe.media_id).unwrap().is_some());
}
#[test]
fn collection_delete_preserves_bounded_progress_after_later_candidate_failure() {
    let (temp, store, subscribe) = fixture();
    let collection = store
        .create_collection(subscribe.user_id, "bounded")
        .unwrap();
    let template = store.get_media(subscribe.media_id).unwrap().unwrap();
    let mut last = template.id;
    for index in 1..=65 {
        let mut media = template.clone();
        media.id = format!("00000000-0000-4000-8000-{index:012}")
            .parse()
            .unwrap();
        store.insert_media(&media).unwrap();
        store
            .add_collection_item(&collection, &media.id.to_string())
            .unwrap();
        last = media.id;
    }
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER block_last_collection BEFORE DELETE ON collection_items WHEN OLD.media_id='{last}' BEGIN SELECT RAISE(FAIL,'fixture last candidate'); END;")).unwrap();
    assert!(store.delete_collection(&collection).is_err());
    assert!(
        store
            .list_collections(subscribe.user_id)
            .unwrap()
            .is_empty(),
        "deleting Collection must stay hidden"
    );
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM collection_items WHERE collection_id=?1",
            [&collection],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "first bounded window must remain committed");
    db.execute_batch("DROP TRIGGER block_last_collection")
        .unwrap();
    Store::open(temp.path())
        .unwrap()
        .delete_collection(&collection)
        .unwrap();
    assert!(store.get_media(last).unwrap().is_none());
}
#[test]
fn prefrozen_retained_creator_cannot_commit_after_merge_repoint_and_restart() {
    let (temp, store, subscribe) = fixture();
    let mut survivor = store.get_media(subscribe.media_id).unwrap().unwrap();
    survivor.id = MediaId::new();
    store.insert_media(&survivor).unwrap();
    let creating = store.begin_subscribe_creation(&subscribe).unwrap();
    store.retain_subscribe_creation(&creating).unwrap();
    let request = store::playback::MediaMergeRequest {
        request_id: SubscribeId::new().to_string(),
        survivor: survivor.id,
        loser: subscribe.media_id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: Default::default(),
    };
    let op = store.begin_media_merge(&request, 1).unwrap();
    store.advance_media_merge(&op, 2).unwrap();
    assert!(
        store
            .commit_subscribe_creation(&creating, &subscribe, false)
            .is_err(),
        "late creator cannot commit to retired loser"
    );
    Store::open(temp.path())
        .unwrap()
        .recover_subscribe_references(64)
        .unwrap();
    assert!(store.get_subscribe(subscribe.id).unwrap().is_none());
}
#[test]
fn independent_handle_second_begin_is_rejected_and_first_generation_stays_valid() {
    let (temp, store, subscribe) = fixture();
    let op = store.begin_subscribe_creation(&subscribe).unwrap();
    let peer = Store::open(temp.path()).unwrap();
    assert!(peer.begin_subscribe_creation(&subscribe).is_err());
    store.retain_subscribe_creation(&op).unwrap();
    assert!(peer.begin_subscribe_creation(&subscribe).is_err());
    store
        .commit_subscribe_creation(&op, &subscribe, false)
        .unwrap();
    assert!(peer.begin_subscribe_creation(&subscribe).is_err());
    assert!(store.subscribe_job_sync(subscribe.id).unwrap().is_some());
}
#[test]
fn failed_first_reference_recovery_does_not_starve_later_windows() {
    let (temp, store, subscribe) = fixture();
    let collection = store
        .create_collection(subscribe.user_id, "keep media during abort")
        .unwrap();
    store
        .add_collection_item(&collection, &subscribe.media_id.to_string())
        .unwrap();
    let mut candidate = subscribe.clone();
    for _ in 0..65 {
        candidate.id = SubscribeId::new();
        store.begin_subscribe_creation(&candidate).unwrap();
    }
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER poison_abort BEFORE INSERT ON subscribe_retention_fences BEGIN SELECT RAISE(FAIL,'fixture abort failure'); END;").unwrap();
    let _ = store.recover_subscribe_references(64);
    db.execute_batch("DROP TRIGGER poison_abort").unwrap();
    store.recover_subscribe_references(64).unwrap();
    assert_eq!(
        store
            .pending_subscribe_jobs_cleanup(None, 64)
            .unwrap()
            .len(),
        64,
        "first failed window must remain discoverable"
    );
    let state: String = rusqlite::Connection::open(temp.path().join("subscribe.db"))
        .unwrap()
        .query_row(
            "SELECT state FROM subscribe_reference_ops ORDER BY epoch DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        state, "done",
        "later window must be reached despite earlier failures"
    );
}
#[test]
fn app_settings_cleanup_failure_cannot_turn_committed_delete_into_failure() {
    let (temp, store, subscribe) = fixture();
    store.insert_subscribe(&subscribe).unwrap();
    store
        .put_setting(&format!("subscribe.notice:{}", subscribe.id), "yes")
        .unwrap();
    let app = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
    app.execute_batch("CREATE TRIGGER reject_app_cleanup BEFORE DELETE ON settings BEGIN SELECT RAISE(FAIL,'fixture App cleanup'); END;").unwrap();
    assert!(
        store.delete_subscribe(subscribe.id).unwrap(),
        "business deletion committed; App cleanup must remain deferred"
    );
    assert!(store.get_subscribe(subscribe.id).unwrap().is_none());
    assert!(store.get_media(subscribe.media_id).unwrap().is_none());
}
#[test]
fn all_file_cleanup_seams_preserve_retained_media_then_final_release_purges() {
    for cleanup in 0..4 {
        let (temp, store, subscribe) = fixture();
        let root = temp.path().join("files");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "retention files",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let path = root.join("movie.mkv");
        std::fs::write(&path, b"fixture").unwrap();
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: subscribe.media_id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        store.insert_subscribe(&subscribe).unwrap();
        match cleanup {
            0 => {
                store
                    .delete_files(library.id.parse().unwrap(), &[file.file_id])
                    .unwrap();
            }
            1 => {
                let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
                store
                    .apply_ledger_presence(
                        &[store::LedgerPresenceObservation {
                            path_key: row.path_key.clone(),
                            file: row,
                            missing_at: None,
                            presence: store::FilePresence::Missing,
                        }],
                        store::LedgerPresenceMode::Purge,
                        100,
                    )
                    .unwrap();
            }
            2 => {
                store
                    .enqueue_file_deletion_observed(
                        path.to_str().unwrap(),
                        "cleanup",
                        store::file_identity(path.to_str().unwrap()),
                    )
                    .unwrap();
                let entry = store.list_due_file_deletions(i64::MAX).unwrap().remove(0);
                store.complete_file_deletion(&entry).unwrap();
            }
            _ => {
                let removal = store.begin_library_removal(&library.id).unwrap();
                store
                    .set_removal_state(&removal.operation_id, "cleaning", None)
                    .unwrap();
                while store
                    .clean_library_removal(&removal.operation_id)
                    .unwrap()
                    .state
                    != "succeeded"
                {}
            }
        }
        assert!(
            store.get_media(subscribe.media_id).unwrap().is_some(),
            "cleanup {cleanup} must use Library retention"
        );
        assert!(store.get_subscribe(subscribe.id).unwrap().is_some());
        store.delete_subscribe(subscribe.id).unwrap();
        assert!(
            store.get_media(subscribe.media_id).unwrap().is_none(),
            "final release after cleanup {cleanup}"
        );
    }
}
#[test]
fn ensure_rollback_and_last_file_cleanup_respect_retention_at_each_create_boundary() {
    for boundary in 0..3 {
        let (temp, store, subscribe) = fixture();
        let expected = store.get_media(subscribe.media_id).unwrap().unwrap();
        let op = store.begin_subscribe_creation(&subscribe).unwrap();
        if boundary > 0 {
            store.retain_subscribe_creation(&op).unwrap();
        }
        if boundary > 1 {
            store
                .commit_subscribe_creation(&op, &subscribe, false)
                .unwrap();
        }
        let peer = Store::open(temp.path()).unwrap();
        let removed = peer
            .undo_ensured_media_if_unreferenced(subscribe.media_id, &expected)
            .unwrap();
        assert_eq!(removed, boundary == 0);
        if boundary == 0 {
            assert!(store.retain_subscribe_creation(&op).is_err());
        }
        if boundary == 1 {
            store
                .commit_subscribe_creation(&op, &subscribe, false)
                .unwrap();
        }
        if boundary > 0 {
            assert!(peer.get_media(subscribe.media_id).unwrap().is_some());
        }
    }
}
#[test]
fn merge_failure_reopen_delete_and_frozen_candidates_keep_recovery_discoverable() {
    let (temp, store, subscribe) = fixture();
    store.insert_subscribe(&subscribe).unwrap();
    let mut survivor = store.get_media(subscribe.media_id).unwrap().unwrap();
    survivor.id = MediaId::new();
    store.insert_media(&survivor).unwrap();
    let request = store::playback::MediaMergeRequest {
        request_id: SubscribeId::new().to_string(),
        survivor: survivor.id,
        loser: subscribe.media_id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: Default::default(),
    };
    let op = store.begin_media_merge(&request, 1).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER block_repoint BEFORE UPDATE OF media_id ON subscribes BEGIN SELECT RAISE(FAIL,'fixture repoint crash'); END;").unwrap();
    assert!(store.advance_media_merge(&op, 2).is_err());
    let reopened = Store::open(temp.path()).unwrap();
    assert!(reopened.get_media(survivor.id).unwrap().is_some());
    reopened.delete_subscribe(subscribe.id).unwrap();
    assert_eq!(
        reopened.recover_orphan_media().unwrap(),
        0,
        "frozen candidates wait for merge finish"
    );
    db.execute_batch("DROP TRIGGER block_repoint").unwrap();
    reopened.advance_media_merge(&op, 3).unwrap();
    assert!(
        reopened.recover_orphan_media().unwrap() > 0,
        "frozen candidates must survive until unfreeze"
    );
    assert!(reopened.get_subscribe(subscribe.id).unwrap().is_none());
    assert_eq!(
        reopened.media_operation_status(&op).unwrap().state,
        "succeeded"
    );
}
#[test]
fn deleting_another_subscribe_does_not_abort_an_active_creator() {
    let (temp, store, subscribe) = fixture();
    let mut other = subscribe.clone();
    other.id = SubscribeId::new();
    store.insert_subscribe(&other).unwrap();
    let creator = store.begin_subscribe_creation(&subscribe).unwrap();
    store.retain_subscribe_creation(&creator).unwrap();
    Store::open(temp.path())
        .unwrap()
        .delete_subscribe(other.id)
        .unwrap();
    store
        .commit_subscribe_creation(&creator, &subscribe, false)
        .unwrap();
    assert!(store.get_subscribe(subscribe.id).unwrap().is_some());
}
#[test]
fn collection_recovery_skips_poisoned_first_window_without_losing_candidates() {
    let (temp, store, subscribe) = fixture();
    let template = store.get_media(subscribe.media_id).unwrap().unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER poison_collection BEFORE DELETE ON collection_items BEGIN SELECT RAISE(FAIL,'fixture Collection failure'); END;").unwrap();
    let mut entries = Vec::new();
    for _ in 0..65 {
        let mut media = template.clone();
        media.id = MediaId::new();
        store.insert_media(&media).unwrap();
        let collection = store
            .create_collection(subscribe.user_id, "pending")
            .unwrap();
        store
            .add_collection_item(&collection, &media.id.to_string())
            .unwrap();
        assert!(store.delete_collection(&collection).is_err());
        entries.push((collection, media.id));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let (last, last_media) = entries.last().unwrap();
    db.execute_batch(&format!("DROP TRIGGER poison_collection; CREATE TRIGGER poison_collection BEFORE DELETE ON collection_items WHEN OLD.collection_id<>'{last}' BEGIN SELECT RAISE(FAIL,'fixture poisoned first window'); END;")).unwrap();
    store.recover_collection_deletions().unwrap();
    store.recover_collection_deletions().unwrap();
    assert!(
        store.get_media(*last_media).unwrap().is_none(),
        "later Collection must recover despite poisoned first64"
    );
    assert!(store.get_media(entries[0].1).unwrap().is_some());
}
#[test]
fn ensure_rollback_keeps_collection_reference_and_frozen_media() {
    for frozen in [true, false] {
        let (_temp, store, subscribe) = fixture();
        let expected = store.get_media(subscribe.media_id).unwrap().unwrap();
        if frozen {
            let mut survivor = expected.clone();
            survivor.id = MediaId::new();
            store.insert_media(&survivor).unwrap();
            store
                .begin_media_merge(
                    &store::playback::MediaMergeRequest {
                        request_id: SubscribeId::new().to_string(),
                        survivor: survivor.id,
                        loser: expected.id,
                        survivor_revision: 0,
                        loser_revision: 0,
                        refresh_targets: Default::default(),
                    },
                    1,
                )
                .unwrap();
        } else {
            let collection = store.create_collection(subscribe.user_id, "keep").unwrap();
            store
                .add_collection_item(&collection, &expected.id.to_string())
                .unwrap();
        }
        assert!(
            !store
                .undo_ensured_media_if_unreferenced(expected.id, &expected)
                .unwrap(),
            "ensure rollback must respect Collection/frozen authority"
        );
        assert!(store.get_media(expected.id).unwrap().is_some());
    }
}
#[test]
fn merge_preserves_existing_overlapping_policies_and_future_admission_stays_strict() {
    let (_temp, store, loser_subscribe) = fixture();
    let mut media = store.get_media(loser_subscribe.media_id).unwrap().unwrap();
    media.id = MediaId::new();
    store.insert_media(&media).unwrap();
    let mut survivor_subscribe = loser_subscribe.clone();
    survivor_subscribe.id = SubscribeId::new();
    survivor_subscribe.media_id = media.id;
    survivor_subscribe.tracking_state = "paused".into();
    store
        .insert_nonoverlapping_subscribe(&loser_subscribe)
        .unwrap();
    store
        .insert_nonoverlapping_subscribe(&survivor_subscribe)
        .unwrap();
    let old = store
        .subscribe_job_sync(loser_subscribe.id)
        .unwrap()
        .unwrap();
    let survivor_old = store
        .subscribe_job_sync(survivor_subscribe.id)
        .unwrap()
        .unwrap();
    let request = store::playback::MediaMergeRequest {
        request_id: SubscribeId::new().to_string(),
        survivor: media.id,
        loser: loser_subscribe.media_id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: Default::default(),
    };
    let op = store.begin_media_merge(&request, 1).unwrap();
    store.advance_media_merge(&op, 2).unwrap();
    for (id, generation, epoch) in [
        (loser_subscribe.id, old.generation, old.epoch),
        (
            survivor_subscribe.id,
            survivor_old.generation,
            survivor_old.epoch,
        ),
    ] {
        let current = store.subscribe_job_sync(id).unwrap().unwrap();
        assert_eq!(current.subscribe.media_id, media.id);
        assert_eq!(current.generation, generation);
        assert_eq!(current.epoch, epoch);
    }
    assert_eq!(
        store
            .get_subscribe(survivor_subscribe.id)
            .unwrap()
            .unwrap()
            .tracking_state,
        "paused"
    );
    let mut conflict = survivor_subscribe.clone();
    conflict.id = SubscribeId::new();
    assert!(store.insert_nonoverlapping_subscribe(&conflict).is_err());
    store.delete_subscribe(loser_subscribe.id).unwrap();
    assert!(store.get_media(media.id).unwrap().is_some());
    store.delete_subscribe(survivor_subscribe.id).unwrap();
    assert_eq!(
        store.media_operation_status(&op).unwrap().state,
        "succeeded"
    );
}
#[test]
fn release_does_not_ack_before_all_generation_refs_are_removed() {
    let (temp, store, subscribe) = fixture();
    store.insert_subscribe(&subscribe).unwrap();
    let intent = store.subscribe_job_sync(subscribe.id).unwrap().unwrap();
    let template = store.get_media(subscribe.media_id).unwrap().unwrap();
    let mut ids = vec![template.id];
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    for _ in 0..65 {
        let mut media = template.clone();
        media.id = MediaId::new();
        store.insert_media(&media).unwrap();
        db.execute(
            "INSERT INTO subscribe_retention VALUES(?1,?2,?3)",
            rusqlite::params![
                subscribe.id.to_string(),
                intent.generation,
                media.id.to_string()
            ],
        )
        .unwrap();
        ids.push(media.id);
    }
    store.delete_subscribe(subscribe.id).unwrap();
    let peer = Store::open(temp.path()).unwrap();
    peer.recover_subscribe_references(64).unwrap();
    for id in ids {
        assert!(
            peer.get_media(id).unwrap().is_none(),
            "every generation ref must be released before durable completion"
        );
    }
}
#[test]
fn duplicate_merge_drivers_preserve_generation_refs_and_owned_teardown() {
    let (temp, store, subscribe) = fixture();
    store.insert_subscribe(&subscribe).unwrap();
    let mut survivor = store.get_media(subscribe.media_id).unwrap().unwrap();
    survivor.id = MediaId::new();
    store.insert_media(&survivor).unwrap();
    let request = store::playback::MediaMergeRequest {
        request_id: SubscribeId::new().to_string(),
        survivor: survivor.id,
        loser: subscribe.media_id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: Default::default(),
    };
    let op = store.begin_media_merge(&request, 1).unwrap();
    let peer = Store::open(temp.path()).unwrap();
    let other = op.clone();
    let worker = std::thread::spawn(move || peer.advance_media_merge(&other, 2));
    store.advance_media_merge(&op, 3).unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(
        store.media_operation_status(&op).unwrap().state,
        "succeeded"
    );
    let current = store.get_subscribe(subscribe.id).unwrap().unwrap();
    assert_eq!(current.media_id, survivor.id);
    let mut fresh = current.clone();
    fresh.id = SubscribeId::new();
    store.insert_subscribe(&fresh).unwrap();
    store.advance_media_merge(&op, 4).unwrap();
    assert!(
        store.get_subscribe(fresh.id).unwrap().is_some(),
        "stale completed driver cannot abort later incarnation"
    );
}
#[test]
fn reopen_after_library_commit_before_retained_ack_aborts_without_business_replay() {
    let (temp, store, subscribe) = fixture();
    let op = store.begin_subscribe_creation(&subscribe).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_retained BEFORE UPDATE OF state ON subscribe_reference_ops WHEN NEW.state='retained' BEGIN SELECT RAISE(FAIL,'fixture retained acknowledgment'); END;").unwrap();
    assert!(store.retain_subscribe_creation(&op).is_err());
    db.execute_batch("DROP TRIGGER fail_retained").unwrap();
    let peer = Store::open(temp.path()).unwrap();
    peer.recover_subscribe_references(64).unwrap();
    assert!(peer.get_subscribe(subscribe.id).unwrap().is_none());
    assert!(store.retain_subscribe_creation(&op).is_err());
    assert!(peer.get_media(subscribe.media_id).unwrap().is_none());
}
#[test]
fn malformed_reference_window_advances_durably_and_retains_corrupt_operations() {
    let (temp, store, subscribe) = fixture();
    let mut candidate = subscribe.clone();
    for _ in 0..65 {
        candidate.id = SubscribeId::new();
        store.begin_subscribe_creation(&candidate).unwrap();
    }
    let db = rusqlite::Connection::open(temp.path().join("subscribe.db")).unwrap();
    db.execute(
        "UPDATE subscribe_reference_ops SET subscribe_id='invalid-'||epoch WHERE epoch<=64",
        [],
    )
    .unwrap();
    assert!(store.recover_subscribe_references(64).is_err());
    let peer = Store::open(temp.path()).unwrap();
    let _ = peer.recover_subscribe_references(64);
    let (state,poisoned):(String,i64)=db.query_row("SELECT (SELECT state FROM subscribe_reference_ops ORDER BY epoch DESC LIMIT 1),(SELECT count(*) FROM subscribe_reference_ops WHERE subscribe_id LIKE 'invalid-%' AND state='creating')",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(
        state, "done",
        "raw durable cursor must advance past malformed first64 after reopen"
    );
    assert_eq!(
        poisoned, 64,
        "malformed operations must fail closed and remain discoverable"
    );
}
#[test]
fn orphan_recovery_passes_poisoned_delete_window_after_reopen() {
    let (temp, store, subscribe) = fixture();
    let template = store.get_media(subscribe.media_id).unwrap().unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    let mut ids = Vec::new();
    for index in 1..=65 {
        let mut media = template.clone();
        media.id = format!("00000000-0000-4000-8000-{index:012}")
            .parse()
            .unwrap();
        store.insert_media(&media).unwrap();
        db.execute(
            "INSERT INTO pending_orphan_media VALUES(?1)",
            [media.id.to_string()],
        )
        .unwrap();
        ids.push(media.id);
    }
    db.execute_batch(&format!("CREATE TRIGGER poison_orphan BEFORE DELETE ON media WHEN OLD.media_id='{}' BEGIN SELECT RAISE(FAIL,'fixture orphan purge failure'); END;",ids[0])).unwrap();
    let _ = store.recover_orphan_media();
    let peer = Store::open(temp.path()).unwrap();
    let _ = peer.recover_orphan_media();
    assert!(
        peer.get_media(ids[64]).unwrap().is_none(),
        "later orphan must progress past poisoned first window"
    );
    assert!(
        peer.get_media(ids[0]).unwrap().is_some(),
        "failed orphan must retain durable candidate"
    );
}
#[test]
fn malformed_orphan_candidate_fails_closed_but_does_not_starve_valid_work() {
    let (temp, store, subscribe) = fixture();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    db.execute_batch("INSERT INTO media(media_id,kind,title,identity_state) VALUES('!invalid-media','movie','corrupt candidate','provisional'); INSERT INTO pending_orphan_media VALUES('!invalid-media');").unwrap();
    db.execute(
        "INSERT INTO pending_orphan_media VALUES(?1)",
        [subscribe.media_id.to_string()],
    )
    .unwrap();
    let _ = store.recover_orphan_media();
    let peer = Store::open(temp.path()).unwrap();
    let _ = peer.recover_orphan_media();
    assert!(
        peer.get_media(subscribe.media_id).unwrap().is_none(),
        "valid candidate must progress past malformed raw identity"
    );
    let pending: i64 = db
        .query_row(
            "SELECT count(*) FROM pending_orphan_media WHERE media_id='!invalid-media'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1);
}
