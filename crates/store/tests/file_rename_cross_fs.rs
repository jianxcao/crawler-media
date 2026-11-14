//! A7: a real cross-filesystem rename (disk `/tmp` → tmpfs `/dev/shm`). `rename(2)`
//! fails with EXDEV, so the disk phase must take the copy path for real, record by
//! receipt and recover by identity after a crash before record.
use domain::{Media, MediaId, MediaKind};
use std::path::{Path, PathBuf};
use store::Store;
use store::library_write::execute_rename_disk;
use store::test_support::{FixtureFile, publish_fixture_file};

fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(a).unwrap().dev() == std::fs::metadata(b).unwrap().dev()
}

struct Fx {
    _disk: tempfile::TempDir,
    _shm: tempfile::TempDir,
    store: Store,
    shm_root: PathBuf,
    file: domain::LibraryFile,
}

fn fixture() -> Option<Fx> {
    let disk = tempfile::tempdir().unwrap();
    let shm = tempfile::tempdir_in("/dev/shm").ok()?;
    let disk_root = disk.path().join("a");
    let shm_root = shm.path().join("b");
    std::fs::create_dir_all(&disk_root).unwrap();
    std::fs::create_dir_all(&shm_root).unwrap();
    if same_device(&disk_root, &shm_root) {
        return None;
    }
    let store = Store::open(disk.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "A",
            &[disk_root.to_str().unwrap(), shm_root.to_str().unwrap()],
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
    let path = disk_root.join("M.2000.mkv");
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
    Some(Fx {
        _disk: disk,
        _shm: shm,
        store,
        shm_root,
        file,
    })
}

fn staging_leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".rename-"))
        .collect()
}

#[test]
fn disk_to_tmpfs_rename_copies_records_and_cleans_staging() {
    let Some(fx) = fixture() else {
        eprintln!("skipped: /dev/shm is not a separate filesystem here");
        return;
    };
    let to = fx.shm_root.join("Moved.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    let receipt = execute_rename_disk(&req).unwrap();
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    let after = fx
        .store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.file.path, to);
    // A copy is a new physical version (new dev/ino): exactly one revision bump,
    // and replaying the same receipt does not bump again.
    assert_eq!(after.file.version.revision, fx.file.version.revision + 1);
    let _ = fx.store.record_rename_disk(&receipt, 4);
    let replayed = fx
        .store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(replayed.file.version.revision, after.file.version.revision);
    assert!(!fx.file.path.exists());
    assert_eq!(std::fs::read(&to).unwrap(), b"movie bytes");
    assert!(staging_leftovers(&fx.shm_root).is_empty());
}

#[test]
fn disk_to_tmpfs_crash_before_record_recovers_by_identity() {
    let Some(fx) = fixture() else {
        eprintln!("skipped: /dev/shm is not a separate filesystem here");
        return;
    };
    let to = fx.shm_root.join("Moved.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    execute_rename_disk(&req).unwrap();
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    let after = fx
        .store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.file.path, to);
    assert_eq!(std::fs::read(&to).unwrap(), b"movie bytes");
    assert!(staging_leftovers(&fx.shm_root).is_empty());
}
