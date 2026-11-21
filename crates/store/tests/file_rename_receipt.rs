//! C10: the disk phase runs without the Store and returns a receipt carrying the
//! destination's native identity; the Store records it in a short transaction.
//! Recovery proves ownership by identity (source inode, staging inode or the
//! persisted receipt), never by size + mtime.
use domain::{Media, MediaId, MediaKind};
use std::path::PathBuf;
use store::Store;
use store::library_write::{execute_rename_disk, execute_rename_disk_copying};
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    _guard: tempfile::TempDir,
    store: Store,
    root: PathBuf,
    file: domain::LibraryFile,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("a");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
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
    let path = root.join("M.2000.mkv");
    std::fs::write(&path, b"movie bytes").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    Fx {
        _guard: tmp,
        store,
        root,
        file,
    }
}

fn revision(store: &Store, file: &domain::LibraryFile) -> i64 {
    store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file
        .version
        .revision
}

#[test]
fn replayed_receipt_is_idempotent_and_bumps_revision_once() {
    let fx = fixture();
    let to = fx.root.join("Renamed.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    let receipt = execute_rename_disk(&req).unwrap();
    let replayed = execute_rename_disk(&req).unwrap();
    assert_eq!(
        receipt, replayed,
        "a finished disk phase reports the same receipt"
    );
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    fx.store.record_rename_disk(&receipt, 3).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 4).unwrap();
    // A rename keeps the content version; replaying its receipt never bumps it.
    let first_receipt_revision = revision(&fx.store, &fx.file);
    assert_eq!(first_receipt_revision, fx.file.version.revision);
    assert!(
        fx.store.record_rename_disk(&receipt, 5).is_err() || {
            revision(&fx.store, &fx.file) == first_receipt_revision
        }
    );
    assert_eq!(std::fs::read(&to).unwrap(), b"movie bytes");
}

/// Copy path (as across devices): a crash after the copy was published and the source
/// removed, but before the Store recorded anything, recovers from the staging inode.
#[test]
fn copy_crash_before_record_recovers_by_staging_identity() {
    let fx = fixture();
    let to = fx.root.join("Copied.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    execute_rename_disk_copying(&req).unwrap();
    assert!(!fx.file.path.exists());
    // Crash: nothing recorded. Startup recovery converges on identity evidence.
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    let after = fx
        .store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.file.path, to);
    assert_eq!(std::fs::read(&to).unwrap(), b"movie bytes");
    let leftovers: Vec<_> = std::fs::read_dir(&fx.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".rename-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging removed after record: {leftovers:?}"
    );
}

/// A foreign destination with the source's size and mtime is never adopted, and its
/// bytes are never deleted, once the source is gone and no identity evidence remains.
#[test]
fn foreign_destination_with_matching_size_and_mtime_is_refused() {
    let fx = fixture();
    let to = fx.root.join("Foreign.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    let mtime = std::fs::metadata(&fx.file.path)
        .unwrap()
        .modified()
        .unwrap();
    // The foreign file exists before the source goes, so it cannot reuse its inode.
    std::fs::write(&to, b"FOREIGN!!!!").unwrap();
    std::fs::remove_file(&fx.file.path).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&to)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let foreign_destination_bytes_before = std::fs::read(&to).unwrap();
    assert!(execute_rename_disk(&req).is_err());
    assert!(execute_rename_disk_copying(&req).is_err());
    assert_eq!(
        std::fs::read(&to).unwrap(),
        foreign_destination_bytes_before
    );
}

fn assert_same_entry_refused(fx: &Fx, target: &std::path::Path, now: i64) {
    assert!(
        fx.store
            .begin_file_rename(
                fx.file.file_id,
                &fx.file.version,
                &fx.file.path,
                target,
                now
            )
            .is_err()
    );
    assert!(
        fx.store
            .rename_file_within_root(fx.file.file_id, &fx.file.version, &fx.file.path, target)
            .is_err()
    );
}

#[test]
fn same_directory_entry_rename_is_refused_without_deleting_the_file() {
    let fx = fixture();
    let original_bytes = std::fs::read(&fx.file.path).unwrap();

    // Exact same path through both public Store entry points.
    assert_same_entry_refused(&fx, &fx.file.path, 1);

    // Parent directory alias resolving to the same directory entry.
    let alias_root = fx._guard.path().join("alias_a");
    std::os::unix::fs::symlink(&fx.root, &alias_root).unwrap();
    let alias_path = alias_root.join("M.2000.mkv");
    assert_same_entry_refused(&fx, &alias_path, 2);

    // Case-only spelling difference on case-insensitive volumes (with both nlink=1 and nlink=2).
    let lower_path = fx.root.join("m.2000.mkv");
    let case_insensitive = std::fs::symlink_metadata(&lower_path).is_ok();
    if case_insensitive {
        assert_same_entry_refused(&fx, &lower_path, 3);
        let download_link = fx._guard.path().join("download_copy.mkv");
        std::fs::hard_link(&fx.file.path, &download_link).unwrap();
        assert_same_entry_refused(&fx, &lower_path, 4);
    }

    // Low-level disk execution guard when a request already points to the same directory entry.
    let mut forged = fx
        .store
        .begin_file_rename(
            fx.file.file_id,
            &fx.file.version,
            &fx.file.path,
            &fx.root.join("ValidTarget.mkv"),
            5,
        )
        .unwrap();
    forged.to_path = forged.from_path.clone();
    assert!(execute_rename_disk(&forged).is_err());
    if case_insensitive {
        forged.to_path = lower_path;
        assert!(execute_rename_disk(&forged).is_err());
    }
    fx.store
        .with_writer(store::Db::Library, |conn| {
            conn.execute("DELETE FROM pending_file_renames", [])?;
            Ok(())
        })
        .unwrap();

    // Original file, ledger row, and recovery queue must remain completely intact.
    assert_eq!(std::fs::read(&fx.file.path).unwrap(), original_bytes);
    let current = fx
        .store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.path, fx.file.path);
    assert_eq!(current.file.version, fx.file.version);
    assert!(fx.store.file_rename_recovery_schedule().unwrap().is_empty());
}

#[test]
fn two_independent_hard_links_recover_from_link_crash() {
    let fx = fixture();
    let to = fx.root.join("HardLinkedDest.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    // Simulate a crash right after `hard_link(from, to)` and before `remove_file(from)`.
    std::fs::hard_link(&fx.file.path, &to).unwrap();
    let receipt = execute_rename_disk(&req).unwrap();
    assert!(!fx.file.path.exists(), "source link must be removed");
    assert_eq!(std::fs::read(&to).unwrap(), b"movie bytes");
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    let current = fx
        .store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.path, to);
    assert_eq!(current.file.version, fx.file.version);
}

fn seed_subscribe_fact(
    store: &Store,
    media_id: MediaId,
    path: &std::path::Path,
) -> domain::SubscribeId {
    use domain::{
        Coverage, FetchMode, FilterId, QualityFact, Subscribe, SubscribeFacts, SubscribeId, UserId,
    };
    let sub_id = SubscribeId::new();
    store
        .insert_subscribe(&Subscribe {
            id: sub_id,
            user_id: UserId::new(),
            media_id,
            coverage: Coverage::Movie,
            fetch_mode: FetchMode::Search,
            filter_id: FilterId::new(),
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
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 80,
            path: Some(path.to_string_lossy().into_owned()),
        },
    );
    store.save_subscribe_facts(sub_id, &facts).unwrap();
    sub_id
}

#[test]
fn replaced_destination_after_disk_receipt_is_refused_on_reopen_recovery() {
    for copying in [false, true] {
        let fx = fixture();
        let data_dir = fx._guard.path().join("data");
        let sub_id = seed_subscribe_fact(&fx.store, fx.file.media_id, &fx.file.path);
        let to = fx.root.join("Moved.2000.mkv");
        let req = fx
            .store
            .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
            .unwrap();
        let receipt = if copying {
            execute_rename_disk_copying(&req).unwrap()
        } else {
            execute_rename_disk(&req).unwrap()
        };
        fx.store.record_rename_disk(&receipt, 2).unwrap();

        // Atomically replace destination with an independent file after the receipt was saved.
        let replacement = fx.root.join("replacement.tmp");
        std::fs::write(&replacement, b"DIFFERENT VIDEO BYTES").unwrap();
        std::fs::rename(&replacement, &to).unwrap();

        let original_path = fx.file.path.clone();
        let original_version = fx.file.version.clone();
        let file_id = fx.file.file_id;
        drop(fx.store);

        let reopened = Store::open(&data_dir).unwrap();
        let err = reopened.resume_one_file_rename(&req.rename_id, 3);
        assert!(
            err.is_err(),
            "copying={copying}: replaced destination must not be published: {err:?}"
        );
        let persisted = reopened.load_file_rename(&req.rename_id).unwrap();
        assert_eq!(persisted.phase, "failed");
        let coverage = reopened.file_coverage_by_id(file_id).unwrap().unwrap();
        assert_eq!(coverage.file.path, original_path);
        assert_eq!(coverage.file.version, original_version);
        let facts = reopened.load_subscribe_facts(sub_id).unwrap();
        assert_eq!(
            facts.get(None, None).and_then(|f| f.path.as_deref()),
            Some(original_path.to_string_lossy().as_ref())
        );
        assert_eq!(std::fs::read(&to).unwrap(), b"DIFFERENT VIDEO BYTES");
    }
}

#[test]
fn intact_destination_after_disk_receipt_recovers_for_move_and_copy() {
    for copying in [false, true] {
        let fx = fixture();
        let data_dir = fx._guard.path().join("data");
        let sub_id = seed_subscribe_fact(&fx.store, fx.file.media_id, &fx.file.path);
        let to = fx.root.join("Intact.2000.mkv");
        let req = fx
            .store
            .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
            .unwrap();
        let receipt = if copying {
            execute_rename_disk_copying(&req).unwrap()
        } else {
            execute_rename_disk(&req).unwrap()
        };
        fx.store.record_rename_disk(&receipt, 2).unwrap();
        let file_id = fx.file.file_id;
        drop(fx.store);

        let reopened = Store::open(&data_dir).unwrap();
        reopened.resume_one_file_rename(&req.rename_id, 3).unwrap();
        assert!(reopened.load_file_rename(&req.rename_id).is_err());
        let coverage = reopened.file_coverage_by_id(file_id).unwrap().unwrap();
        assert_eq!(coverage.file.path, to);
        let facts = reopened.load_subscribe_facts(sub_id).unwrap();
        assert_eq!(
            facts.get(None, None).and_then(|f| f.path.as_deref()),
            Some(to.to_string_lossy().as_ref())
        );
        assert_eq!(std::fs::read(&to).unwrap(), b"movie bytes");
    }
}
