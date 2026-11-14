//! A7: rename disk record split into lock-free IO phases.
use domain::{Media, MediaId, MediaKind};
use std::path::{Path, PathBuf};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    _tmp: tempfile::TempDir,
    store: Store,
    a: PathBuf,
    b: PathBuf,
    media: MediaId,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    for (name, root) in [("A", &a), ("B", &b)] {
        std::fs::create_dir_all(root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Show".into(),
            year: Some(2020),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    Fx {
        _tmp: tmp,
        store,
        a,
        b,
        media,
    }
}

fn episode(fx: &Fx, dir: &Path, episode: u32) -> domain::LibraryFile {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("S01E0{episode}.mkv"));
    std::fs::write(&path, b"x").unwrap();
    publish_fixture_file(
        &fx.store,
        &FixtureFile {
            media_id: fx.media,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(episode),
            ..Default::default()
        },
    )
    .unwrap()
}

fn begin(
    fx: &Fx,
    file: &domain::LibraryFile,
    to: &Path,
) -> store::library_write::FileRenameRequest {
    fx.store
        .begin_file_rename(file.file_id, &file.version, &file.path, to, 1)
        .unwrap()
}

/// A7: sidecar observation and sidecar moves run with the Store lock held by someone
/// else; only the short decide / finish steps take it.
#[test]
fn sidecar_io_runs_while_another_holder_owns_the_store_lock() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &file, &to);
    let receipt = store::library_write::execute_rename_disk(&req).unwrap();
    let Fx { _tmp, store, b, .. } = fx;
    let lock = std::sync::Mutex::new(store);
    let plan = lock
        .lock()
        .unwrap()
        .begin_rename_disk_record(&receipt)
        .unwrap()
        .expect("planned intent");
    let held = lock.lock().unwrap();
    let observed = plan.observe();
    drop(held);
    let decided = lock.lock().unwrap().decide_rename_disk(observed).unwrap();
    let held = lock.lock().unwrap();
    // A concurrent reader keeps using the Store during the sidecar move.
    assert!(held.load_file_rename(&req.rename_id).is_ok());
    let applied = decided.apply().unwrap();
    drop(held);
    assert!(
        b.join("Show").join("poster.jpg").is_file(),
        "applied without the Store"
    );
    let store = lock.into_inner().unwrap();
    store.finish_rename_disk(applied, 2).unwrap();
    assert_eq!(
        store.load_file_rename(&req.rename_id).unwrap().phase,
        "disk_applied"
    );
    assert!(
        store.begin_rename_disk_record(&receipt).unwrap().is_none(),
        "replay no-op"
    );
    store.complete_file_rename(&req.rename_id, 3).unwrap();
}

/// A phase that left `planned` between observe and decide is refused, never re-decided.
#[test]
fn stale_observation_is_refused_after_another_recorder_finished() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &file, &to);
    let receipt = store::library_write::execute_rename_disk(&req).unwrap();
    let stale = fx
        .store
        .begin_rename_disk_record(&receipt)
        .unwrap()
        .unwrap()
        .observe();
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    assert!(fx.store.decide_rename_disk(stale).is_err());
}

#[test]
fn intent_and_completion_observe_files_without_borrowing_the_store() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("S01E01.nfo"), b"<episodedetails/>").unwrap();
    let to = fx.b.join("Show/S01E01.mkv");
    let plan = fx
        .store
        .plan_file_rename(file.file_id, &file.version, &file.path, &to)
        .unwrap();
    let lock = std::sync::Mutex::new(fx.store);
    let held = lock.lock().unwrap();
    let observed = plan.observe().unwrap();
    assert!(held.file_coverage_by_id(file.file_id).unwrap().is_some());
    drop(held);
    let request = lock
        .lock()
        .unwrap()
        .begin_observed_file_rename(observed, 1)
        .unwrap();
    assert!(request.sidecars.iter().any(|side| side.kind == "nfo"));
    let receipt = store::library_write::execute_rename_disk(&request).unwrap();
    let plan = lock
        .lock()
        .unwrap()
        .begin_rename_disk_record(&receipt)
        .unwrap()
        .unwrap();
    let observed = plan.observe();
    let decided = lock.lock().unwrap().decide_rename_disk(observed).unwrap();
    let applied = decided.apply().unwrap();
    lock.lock().unwrap().finish_rename_disk(applied, 2).unwrap();
    let plan = lock
        .lock()
        .unwrap()
        .plan_file_rename_completion(&request.rename_id)
        .unwrap();
    let held = lock.lock().unwrap();
    let observed = plan.observe().unwrap();
    assert_eq!(
        held.load_file_rename(&request.rename_id).unwrap().phase,
        "disk_applied"
    );
    drop(held);
    lock.lock()
        .unwrap()
        .complete_observed_file_rename(observed, 3)
        .unwrap();
    assert_eq!(
        lock.lock()
            .unwrap()
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .path,
        to
    );
    assert!(!file.path.exists());
}

#[test]
fn changed_root_scope_refuses_an_observed_rename_before_any_file_moves() {
    let fx = fixture();
    let file = episode(&fx, &fx.a.join("Show"), 1);
    let to = fx.b.join("Nested/S01E01.mkv");
    let observed = fx
        .store
        .plan_file_rename(file.file_id, &file.version, &file.path, &to)
        .unwrap()
        .observe()
        .unwrap();
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    fx.store
        .create_library(
            MediaKind::Tv,
            "New child",
            &[to.parent().unwrap().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert!(fx.store.begin_observed_file_rename(observed, 1).is_err());
    assert!(file.path.is_file());
    assert!(!to.exists());
}

#[test]
fn source_directory_replaced_by_an_escaping_symlink_cannot_be_renamed() {
    let fx = fixture();
    let directory = fx.a.join("Show");
    let file = episode(&fx, &directory, 1);
    let outside = fx._tmp.path().join("outside");
    std::fs::rename(&directory, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &directory).unwrap();
    let to = fx.b.join("Show/S01E01.mkv");
    let observed = fx
        .store
        .plan_file_rename(file.file_id, &file.version, &file.path, &to)
        .unwrap()
        .observe()
        .unwrap();
    assert!(
        fx.store.begin_observed_file_rename(observed, 1).is_err(),
        "a matching inode outside the recorded Library is still not owned"
    );
    assert!(outside.join("S01E01.mkv").is_file());
    assert!(!to.exists());
}

#[test]
fn recovery_reads_only_the_requested_window_of_due_intents() {
    let fx = fixture();
    for episode_number in 1..=3 {
        let file = episode(&fx, &fx.a.join("Show"), episode_number);
        begin(&fx, &file, &fx.b.join(file.path.file_name().unwrap()));
    }
    let all = fx.store.list_due_file_rename_ids(2).unwrap();
    let window = fx.store.list_due_file_rename_ids_limited(2, 2).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(window, all[..2]);
    assert!(
        fx.store
            .list_due_file_rename_ids_limited(2, 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn crash_after_unlocked_staging_cleanup_recovers_from_the_persisted_receipt() {
    let fx = fixture();
    let file = episode(&fx, &fx.a.join("Show"), 1);
    let to = fx.b.join("Show/S01E01.mkv");
    let request = begin(&fx, &file, &to);
    let receipt = store::library_write::execute_rename_disk(&request).unwrap();
    let observed = fx
        .store
        .begin_rename_disk_record(&receipt)
        .unwrap()
        .unwrap()
        .observe();
    let decided = fx.store.decide_rename_disk(observed).unwrap();
    // The process disappears after sidecars/staging IO, before finish_rename_disk.
    drop(decided.apply().unwrap());
    let pending = fx.store.load_file_rename(&request.rename_id).unwrap();
    assert_eq!(pending.phase, "planned");
    assert!(pending.disk_receipt.is_some());
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    assert_eq!(
        fx.store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .path,
        to
    );
    assert!(to.is_file());
    assert!(!file.path.exists());
}
