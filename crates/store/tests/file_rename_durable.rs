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
    store
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
    store
        .create_library(
            MediaKind::Movie,
            "B",
            &[other.to_str().unwrap()],
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
fn durable_same_root_rename_moves_sidecar_and_survives_crash_after_disk() {
    let fx = fixture();
    let to = fx.a.join("Renamed.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.file.file_id, &fx.file.version, &fx.file.path, &to, 1)
        .unwrap();
    assert_eq!(req.sidecars.len(), 1);
    assert_eq!(req.phase, "planned");
    store::library_write::move_rename_bytes(&req).unwrap();
    assert!(to.is_file());
    assert!(!fx.file.path.is_file());
    assert!(fx.a.join("Renamed.2000.nfo").is_file());
    // Crash window: disk done, ledger still at from. Resume finishes.
    assert_eq!(recorded(&fx.store, fx.file.file_id).path, fx.file.path);
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    let after = recorded(&fx.store, fx.file.file_id);
    assert_eq!(after.path, to);
    assert!(fx.store.list_pending_file_rename_ids().unwrap().is_empty());
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
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
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
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
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
