//! Risk 4: directory-level artwork (`poster.jpg`, `fanart.jpg`, …) follows a
//! renamed / re-parented video only when that video is its sole owner; artwork
//! shared with other recorded media is copied to the destination and kept at the
//! source. Both paths go through the persisted sidecar intent.
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
) -> store::library_write::rename_durable::FileRenameRequest {
    fx.store
        .begin_file_rename(file.file_id, &file.version, &file.path, to, 1)
        .unwrap()
}

#[test]
fn sole_owner_moves_directory_artwork() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    std::fs::write(src.join("fanart.jpg"), b"fanart").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &file, &to);
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    for name in ["poster.jpg", "fanart.jpg"] {
        assert!(
            fx.b.join("Show").join(name).is_file(),
            "{name} follows the sole owner"
        );
        assert!(!src.join(name).exists(), "{name} moved, not copied");
    }
}

#[test]
fn shared_artwork_is_copied_and_kept_for_remaining_media() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let moved = episode(&fx, &src, 1);
    episode(&fx, &src, 2);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &moved, &to);
    // Crash after the intent: recovery must reach the same end state.
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    assert_eq!(
        std::fs::read(src.join("poster.jpg")).unwrap(),
        b"poster",
        "kept for E02"
    );
    assert_eq!(
        std::fs::read(fx.b.join("Show/poster.jpg")).unwrap(),
        b"poster",
        "copied"
    );
    let queued = fx.store.list_pending_file_deletions().unwrap();
    assert!(
        !queued.iter().any(|p| p.ends_with("a/Show/poster.jpg")),
        "shared source artwork must not be queued for deletion: {queued:?}"
    );
    let leftovers: Vec<_> = std::fs::read_dir(fx.b.join("Show"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temp copies left: {leftovers:?}");
    let _ = req;
}

#[test]
fn same_directory_rename_leaves_directory_artwork_alone() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    let req = begin(&fx, &file, &src.join("Show S01E01.mkv"));
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    assert!(src.join("poster.jpg").is_file());
}

#[test]
fn sharing_is_rechecked_when_bytes_move() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let moved = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &moved, &to);
    // A sibling is published after the intent was planned as a sole-owner move.
    episode(&fx, &src, 2);
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    assert!(
        src.join("poster.jpg").is_file(),
        "late sibling keeps the shared poster"
    );
    assert!(fx.b.join("Show/poster.jpg").is_file());
}

#[test]
fn non_standard_artwork_names_follow_the_same_ownership_rules() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    for name in [
        "season01-poster.jpg",
        "show-fanart.png",
        "season-specials-banner.jpg",
        "S01E01-thumb.jpg",
    ] {
        std::fs::write(src.join(name), name).unwrap();
    }
    std::fs::write(src.join("notes.txt"), b"not artwork").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &file, &to);
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    for name in [
        "season01-poster.jpg",
        "show-fanart.png",
        "season-specials-banner.jpg",
        "S01E01-thumb.jpg",
    ] {
        assert!(
            fx.b.join("Show").join(name).is_file(),
            "{name} follows its sole owner"
        );
        assert!(!src.join(name).exists(), "{name} moved");
    }
    assert!(
        src.join("notes.txt").is_file(),
        "non-artwork is never touched"
    );
}

#[test]
fn destination_artwork_conflict_policy_is_explicit() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"source").unwrap();
    std::fs::write(src.join("fanart.jpg"), b"same").unwrap();
    let dest = fx.b.join("Show");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("poster.jpg"), b"destination").unwrap();
    std::fs::write(dest.join("fanart.jpg"), b"same").unwrap();
    let req = begin(&fx, &file, &dest.join("S01E01.mkv"));
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    assert_eq!(
        std::fs::read(dest.join("poster.jpg")).unwrap(),
        b"destination",
        "destination wins"
    );
    assert_eq!(
        std::fs::read(src.join("poster.jpg")).unwrap(),
        b"source",
        "differing source kept"
    );
    let queued = fx.store.list_pending_file_deletions().unwrap();
    assert!(
        !queued.iter().any(|p| p.ends_with("a/Show/poster.jpg")),
        "{queued:?}"
    );
    assert!(
        queued.iter().any(|p| p.ends_with("a/Show/fanart.jpg")) || !src.join("fanart.jpg").exists(),
        "identical source counts as moved: {queued:?}"
    );
}
