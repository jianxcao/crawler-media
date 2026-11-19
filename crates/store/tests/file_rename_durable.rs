//! N03 remainder: durable rename / re-parent with sidecar intents and crash recovery.
use domain::{Media, MediaId, MediaKind};
use std::path::PathBuf;
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    tmp: PathBuf,
    _guard: tempfile::TempDir,
    store: Store,
    a: PathBuf,
    b: PathBuf,
    _media: MediaId,
    file: domain::LibraryFile,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    // One Library with two roots (cross-root same-Library), plus a sibling Library.
    let a_library = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[a.to_str().unwrap(), b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let b_library = store
        .create_library(
            MediaKind::Movie,
            "B",
            &[other.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    // A has two roots, so creation's single batch can leave B's migration queued.
    // Complete fixture ownership before exercising rename publication barriers.
    store.drain_root_reassignments().unwrap();
    for library in [&a_library, &b_library] {
        assert!(
            store
                .root_reassignments_for_library(&library.id)
                .unwrap()
                .iter()
                .all(|operation| operation.state == "succeeded")
        );
    }
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Movie,
            title: "M".into(),
            year: Some(2000),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = a.join("M.2000.mkv");
    std::fs::write(&path, b"m").unwrap();
    std::fs::write(a.join("M.2000.nfo"), b"<movie/>").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let path_tmp = tmp.path().to_path_buf();
    Fx {
        tmp: path_tmp,
        _guard: tmp,
        store,
        a,
        b,
        _media: media,
        file,
    }
}

fn recorded(store: &Store, file_id: domain::LedgerId) -> domain::LibraryFile {
    store.file_coverage_by_id(file_id).unwrap().unwrap().file
}

#[test]
fn recovery_retires_a_rename_whose_file_record_disappeared() {
    let fx = fixture();
    let to = fx.b.join("Moved.mkv");
    let request = begin(&fx, &to);
    assert_eq!(
        fx.store
            .delete_files(request.from_library_id, &[request.file_id])
            .unwrap(),
        1
    );

    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 0);
    assert!(
        fx.store
            .list_due_file_rename_ids(i64::MAX)
            .unwrap()
            .is_empty()
    );
    assert!(fx.file.path.is_file());
    assert!(!to.exists());
}

#[test]
fn removal_can_freeze_either_library_after_rename_publication_finishes() {
    let fx = fixture();
    let to = fx.tmp.join("other").join("Moved.mkv");
    let request = begin(&fx, &to);
    let from_id = request.from_library_id.to_string();
    let to_id = request.to_library_id.to_string();
    assert_ne!(from_id, to_id);

    let publication = fx.store.apply_file_rename(&request, 2).unwrap();
    for library in [&from_id, &to_id] {
        assert!(matches!(
            fx.store.begin_library_removal(library),
            Err(store::StoreError::WriteBusy)
        ));
        assert!(fx.store.library_accepts_work(library).unwrap());
    }
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, to);
    drop(publication);

    fx.store.begin_library_removal(&to_id).unwrap();
    assert!(!fx.store.library_accepts_work(&to_id).unwrap());
    assert!(fx.store.library_accepts_work(&from_id).unwrap());
    assert_eq!(std::fs::read(to).unwrap(), b"m");
}

#[test]
fn durable_same_root_rename_moves_sidecar_and_survives_crash_after_disk() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    assert_eq!(req.sidecars.len(), 1);
    assert_eq!(req.phase, "planned");
    store::library_write::execute_rename_disk(&req).unwrap();
    assert!(to.is_file());
    assert!(!fx.file.path.is_file());
    // Sidecars move only in the locked phase, after the ownership re-check.
    assert!(fx.a.join("M.2000.nfo").is_file());
    // Crash window: disk done, ledger still at from. Resume finishes.
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, fx.file.path);
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    let after = recorded(&fx.store, fx.file.file_id);
    assert_eq!(after.path, to);
    assert!(fx.a.join("Renamed.2000.nfo").is_file());
    assert!(
        fx.store
            .list_due_file_rename_ids(i64::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn durable_cross_root_same_library_reparents_root_id() {
    let fx = fixture();
    let to = fx.b.join("Moved.2000.mkv");
    let before = recorded(&fx.store, fx.file.file_id);
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &before.version, &before.path, &to, 1)
        .unwrap();
    assert_ne!(req.from_root_id, req.to_root_id);
    assert_eq!(req.from_library_id, req.to_library_id);
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| {
            store::library_write::execute_rename_disk(&r).inspect_err(|e| {
                let _ = fx.store.record_rename_refusal(&r.rename_id, 2, e);
            })
        })
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    let after = recorded(&fx.store, fx.file.file_id);
    assert_eq!(after.path, to);
    assert_eq!(after.root_id, req.to_root_id);
    assert_eq!(after.library_id, before.library_id);
    assert!(to.is_file());
}

#[test]
fn durable_cross_library_reparent_updates_library_id() {
    let fx = fixture();
    let other_root = fx.tmp.join("other");
    let to = other_root.join("OtherLib.2000.mkv");
    let before = recorded(&fx.store, fx.file.file_id);
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &before.version, &before.path, &to, 1)
        .unwrap();
    assert_ne!(req.from_library_id, req.to_library_id);
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| {
            store::library_write::execute_rename_disk(&r).inspect_err(|e| {
                let _ = fx.store.record_rename_refusal(&r.rename_id, 2, e);
            })
        })
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    let after = recorded(&fx.store, fx.file.file_id);
    assert_eq!(after.path, to);
    assert_eq!(after.library_id, req.to_library_id);
    assert_ne!(after.library_id, before.library_id);
}

#[test]
fn sync_within_root_api_still_rejects_cross_root() {
    let fx = fixture();
    let to = fx.b.join("Nope.mkv");
    let err = fx
        .store
        .rename_file_within_root(fx.file.file_id, &fx.file.version, &fx.file.path, &to)
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("durable operation") || msg.contains("outside"),
        "{msg}"
    );
    assert!(fx.file.path.is_file());
    assert!(!to.exists());
}

// ---- R01 / R11: disk work bound to source evidence, durable reference phase ----

fn begin(fx: &Fx, to: &std::path::Path) -> store::library_write::FileRenameRequest {
    fx.store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, to, 1)
        .unwrap()
}

fn intent_phase(fx: &Fx, rename_id: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(fx.tmp.join("data/library.db")).unwrap();
    conn.query_row(
        "SELECT phase FROM pending_file_renames WHERE rename_id = ?1",
        [rename_id],
        |r| r.get(0),
    )
    .ok()
}

#[test]
fn replaced_source_is_refused_and_not_moved() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = begin(&fx, &to);
    // Atomic replacement: a different inode with unrelated bytes.
    let staged = fx.a.join(".replacement");
    std::fs::write(&staged, b"replacement").unwrap();
    std::fs::rename(&staged, &fx.file.path).unwrap();
    let stale_disk_result = store::library_write::execute_rename_disk(&req);
    assert!(stale_disk_result.is_err());
    assert_eq!(std::fs::read(&fx.file.path).unwrap(), b"replacement");
    assert!(!to.exists());
    assert!(
        fx.store
            .load_file_rename(&req.rename_id)
            .and_then(
                |r| store::library_write::execute_rename_disk(&r).inspect_err(|e| {
                    let _ = fx.store.record_rename_refusal(&r.rename_id, 2, e);
                })
            )
            .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
            .is_err()
    );
    assert_eq!(intent_phase(&fx, &req.rename_id).as_deref(), Some("failed"));
    assert_eq!(fx.store.resume_pending_file_renames(3).unwrap(), 0);
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, fx.file.path);
    assert_eq!(std::fs::read(&fx.file.path).unwrap(), b"replacement");
}

#[test]
fn foreign_destination_after_source_vanished_is_not_adopted() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = begin(&fx, &to);
    std::fs::remove_file(&fx.file.path).unwrap();
    std::fs::write(&to, b"foreign").unwrap();
    assert!(store::library_write::execute_rename_disk(&req).is_err());
    assert!(
        fx.store
            .load_file_rename(&req.rename_id)
            .and_then(
                |r| store::library_write::execute_rename_disk(&r).inspect_err(|e| {
                    let _ = fx.store.record_rename_refusal(&r.rename_id, 2, e);
                })
            )
            .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
            .is_err()
    );
    assert!(fx.store.complete_file_rename(&req.rename_id, 2).is_err());
    assert_eq!(std::fs::read(&to).unwrap(), b"foreign");
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, fx.file.path);
}

#[test]
fn occupied_destination_is_never_clobbered() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = begin(&fx, &to);
    std::fs::write(&to, b"occupant").unwrap();
    assert!(store::library_write::execute_rename_disk(&req).is_err());
    assert_eq!(std::fs::read(&to).unwrap(), b"occupant");
    assert_eq!(std::fs::read(&fx.file.path).unwrap(), b"m");
}

#[test]
fn complete_requires_durable_disk_phase() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = begin(&fx, &to);
    store::library_write::execute_rename_disk(&req).unwrap();
    // Bytes moved but the disk phase was never recorded: no publish yet.
    assert!(fx.store.complete_file_rename(&req.rename_id, 2).is_err());
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, fx.file.path);
    // Recovery proves the moved inode is ours and finishes.
    assert_eq!(fx.store.resume_pending_file_renames(3).unwrap(), 1);
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, to);
}

fn subscribe_fact_at(fx: &Fx, path: &std::path::Path) -> domain::SubscribeId {
    use domain::{
        Coverage, FetchMode, Filter, FilterAtom, FilterId, Subscribe, SubscribeId, UserId,
    };
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 10,
            rule: domain::AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    fx.store.insert_filter(&filter).unwrap();
    let id = SubscribeId::new();
    fx.store
        .insert_subscribe(&Subscribe {
            id,
            user_id: UserId::new(),
            media_id: fx._media,
            coverage: Coverage::Movie,
            fetch_mode: FetchMode::Search,
            filter_id: filter.id,
            wash_cut: false,
            wash_cut_filter_id: None,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
            keep_old_versions: false,
        })
        .unwrap();
    subscribe_db(fx)
        .execute(
            "INSERT INTO subscribe_facts (subscribe_id, season, episode, score, path)
             VALUES (?1, -1, -1, 10, ?2)",
            rusqlite::params![id.to_string(), path.display().to_string()],
        )
        .unwrap();
    id
}

fn subscribe_db(fx: &Fx) -> rusqlite::Connection {
    rusqlite::Connection::open(fx.tmp.join("data/subscribe.db")).unwrap()
}

fn fact_path(fx: &Fx, id: domain::SubscribeId) -> String {
    subscribe_db(fx)
        .query_row(
            "SELECT path FROM subscribe_facts WHERE subscribe_id = ?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn reference_failure_keeps_intent_and_restart_converges() {
    let fx = fixture();
    let sub = subscribe_fact_at(&fx, &fx.file.path);
    subscribe_db(&fx)
        .execute_batch(
            "CREATE TRIGGER fail_facts BEFORE UPDATE ON subscribe_facts
             BEGIN SELECT RAISE(ABORT, 'injected subscribe failure'); END;",
        )
        .unwrap();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = begin(&fx, &to);
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| {
            store::library_write::execute_rename_disk(&r).inspect_err(|e| {
                let _ = fx.store.record_rename_refusal(&r.rename_id, 2, e);
            })
        })
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
    let reference_failure = fx.store.complete_file_rename(&req.rename_id, 2);
    assert!(reference_failure.is_err());
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, to);
    let pending_intent_after_failure =
        fx.store.list_due_file_rename_ids(i64::MAX).unwrap() == vec![req.rename_id.clone()];
    assert!(pending_intent_after_failure);
    assert_eq!(
        intent_phase(&fx, &req.rename_id).as_deref(),
        Some("library_published")
    );
    subscribe_db(&fx)
        .execute_batch("DROP TRIGGER fail_facts;")
        .unwrap();
    // Restart: a fresh Store resumes from the durable phase.
    let reopened = Store::open(fx.tmp.join("data")).unwrap();
    // The failure scheduled a 60 s back-off (unix seconds); resume once it is due.
    assert_eq!(reopened.resume_pending_file_renames(3).unwrap(), 0);
    assert_eq!(reopened.resume_pending_file_renames(62).unwrap(), 1);
    assert_eq!(fact_path(&fx, sub), to.display().to_string());
    assert!(
        reopened
            .list_due_file_rename_ids(i64::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cross_filesystem_move_copies_and_recovers_by_evidence() {
    use std::os::unix::fs::MetadataExt;
    let fx = fixture();
    let shm = std::path::Path::new("/dev/shm");
    let other_dev = shm.is_dir()
        && std::fs::metadata(shm).unwrap().dev() != std::fs::metadata(&fx.a).unwrap().dev();
    if !other_dev {
        eprintln!("Not run: no second filesystem available for a cross-device rename");
        return;
    }
    let dest_root = tempfile::tempdir_in(shm).unwrap();
    let lib = fx
        .store
        .create_library(
            MediaKind::Movie,
            "Shm",
            &[dest_root.path().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let to = dest_root.path().join("M.2000.mkv");
    let req = begin(&fx, &to);
    assert_eq!(req.to_library_id.to_string(), lib.id);
    store::library_write::execute_rename_disk(&req).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), b"m");
    assert!(!fx.file.path.exists());
    // Recovery recognises the stamped copy on the other device.
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    let after = recorded(&fx.store, fx.file.file_id);
    assert_eq!(after.path, to);
    assert_eq!(
        after.version.revision,
        fx.file.version.revision + 1,
        "copy is new content"
    );
}

#[test]
fn recovery_keeps_intent_retryable_while_library_is_reindexing() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = begin(&fx, &to);
    store::library_write::execute_rename_disk(&req).unwrap();
    assert!(to.is_file());
    assert!(!fx.file.path.exists());

    let lib_id: domain::LibraryId = req.from_library_id;
    let op = fx
        .store
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: "reconfig-lock-rename".into(),
                library_id: lib_id,
                expected_revision: req.from_config_revision,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            1_000,
        )
        .unwrap();
    // Transient `reindexing` must NOT mark a moved rename as permanently `failed`.
    assert!(fx.store.resume_pending_file_renames(2).unwrap() == 0);
    assert_ne!(intent_phase(&fx, &req.rename_id).as_deref(), Some("failed"));
    assert_eq!(
        fx.store.list_due_file_rename_ids(i64::MAX).unwrap(),
        vec![req.rename_id.clone()]
    );

    // Finish reconfiguration (`ready` + bumped `config_revision`), then resume succeeds!
    while fx
        .store
        .advance_library_operation(&op.operation_id, 2_000, 50)
        .unwrap()
        .state
        != "succeeded"
    {}
    assert_eq!(fx.store.resume_pending_file_renames(70).unwrap(), 1);
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, to);
    assert!(
        fx.store
            .list_due_file_rename_ids(i64::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reconfiguration_and_integrity_wait_on_active_rename_publication_and_no_reader_deadlock() {
    let fx = fixture();
    fx.store
        .configure_reader_pool_size(store::Db::Library, std::num::NonZeroUsize::new(1).unwrap())
        .unwrap();
    let to = fx.tmp.join("other").join("Moved.2000.mkv");
    let req = begin(&fx, &to);
    let from_id = req.from_library_id.to_string();
    let publication = fx.store.apply_file_rename(&req, 2).unwrap();

    // Library operations must see active rename_publications and return WriteBusy cleanly.
    assert!(matches!(
        fx.store
            .begin_library_integrity(req.from_library_id, false, "op-busy-1", 1_000),
        Err(store::StoreError::WriteBusy)
    ));
    let new_root = fx.tmp.join("other-new-root");
    std::fs::create_dir_all(&new_root).unwrap();
    assert!(matches!(
        fx.store.begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: "op-busy-2".into(),
                library_id: req.to_library_id,
                expected_revision: req.to_config_revision,
                root_paths: Some(vec![new_root]),
                display_locale: None,
            },
            1_000,
        ),
        Err(store::StoreError::WriteBusy)
    ));
    assert!(fx.store.library_accepts_work(&from_id).unwrap());

    // Dropping `RenamePublication` inside a saturated Library reader pool (size 1)
    // must never deadlock against a concurrent `admit_rename_publication`.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store_ref = &fx.store;
    std::thread::scope(|scope| {
        let holder = scope.spawn(move || {
            store_ref
                .library_read_snapshot(|_db| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
                    drop(publication);
                })
                .unwrap();
        });
        entered_rx.recv().unwrap();
        let file2_path = fx.b.join("Second.2000.mkv");
        std::fs::write(&file2_path, b"s").unwrap();
        let file2 = publish_fixture_file(
            &fx.store,
            &FixtureFile {
                media_id: fx._media,
                path: file2_path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let to2 = fx.b.join("Second-Renamed.2000.mkv");
        let req2 = fx
            .store
            .begin_file_rename(file2.file_id, &file2.version, &file2_path, &to2, 3)
            .unwrap();
        let releaser = scope.spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = release_tx.send(());
        });
        let pub2 = fx.store.apply_file_rename(&req2, 4).unwrap();
        drop(pub2);
        holder.join().unwrap();
        releaser.join().unwrap();
    });
}

#[test]
fn rename_with_changed_destination_bytes_records_obligation_in_milliseconds() {
    let fx = fixture();
    let to = fx.a.join("Replaced.2000.mkv");
    let req = begin(&fx, &to);
    let receipt = store::library_write::execute_rename_disk(&req).unwrap();
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    // Simulate destination bytes changing (cross-device copy / new content) before completion.
    std::fs::write(&to, b"new-video-content").unwrap();
    let now_secs = 1_760_000_000_i64;
    fx.store
        .complete_file_rename(&req.rename_id, now_secs)
        .unwrap();

    let obligations = fx.store.scan_probe_obligations(10).unwrap();
    let ob = obligations
        .into_iter()
        .find(|ob| ob.ledger_id == fx.file.file_id)
        .expect("changed source must emit a probe obligation");
    assert_eq!(ob.created_at_ms, now_secs * 1000);
}

#[test]
fn rename_migrates_nfo_input_key_so_removing_renamed_nfo_clears_ghost_metadata() {
    let fx = fixture();
    let old_nfo = fx.a.join("M.2000.nfo");
    std::fs::write(&old_nfo, "<movie><title>Old NFO Title</title></movie>").unwrap();
    assert!(fx.store.reconcile_nfo_inputs(&fx.file.path).unwrap());
    let detail_before = fx
        .store
        .library_item_detail(fx.file.library_id, fx._media, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(detail_before.item.title, "Old NFO Title");

    let to = fx.a.join("Renamed.2000.mkv");
    let new_nfo = fx.a.join("Renamed.2000.nfo");
    let req = begin(&fx, &to);
    let pub_guard = fx.store.apply_file_rename(&req, 2).unwrap();
    drop(pub_guard);
    assert!(new_nfo.is_file());
    assert!(!old_nfo.exists());

    // Delete the moved NFO and reconcile: no ghost row at old `input_key` may linger.
    std::fs::remove_file(&new_nfo).unwrap();
    assert!(fx.store.reconcile_nfo_inputs(&to).unwrap());
    let detail_after = fx
        .store
        .library_item_detail(fx.file.library_id, fx._media, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(
        detail_after.item.title, "M",
        "deleting the renamed NFO must clear the migrated metadata input without leaving a ghost old-key row"
    );
}
