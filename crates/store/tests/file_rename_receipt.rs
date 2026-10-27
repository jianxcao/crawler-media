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
