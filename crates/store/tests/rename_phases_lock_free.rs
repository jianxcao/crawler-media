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
