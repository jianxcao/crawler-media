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
    fixture_with_alias(false)
}

fn fixture_with_alias(alias: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let a = if alias {
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let real = base.join("real-a");
        std::fs::create_dir_all(&real).unwrap();
        let alias = base.join("alias-a");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        alias
    } else {
        tmp.path().join("a")
    };
    let b = tmp.path().join("b");
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

#[test]
fn sole_owner_moves_directory_artwork() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    std::fs::write(src.join("fanart.jpg"), b"fanart").unwrap();
    let to = fx.b.join("Show").join("S01E01.mkv");
    let req = begin(&fx, &file, &to);
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| store::library_write::execute_rename_disk(&r))
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
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
fn explicit_alias_root_keeps_artwork_used_by_a_sibling_file() {
    let fx = fixture_with_alias(true);
    let src = fx.a.join("Media");
    let moved = episode(&fx, &src, 1);
    episode(&fx, &src, 2);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    begin(&fx, &moved, &fx.b.join("Media/S01E01.mkv"));
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    assert_eq!(std::fs::read(src.join("poster.jpg")).unwrap(), b"poster");
    assert_eq!(
        std::fs::read(fx.b.join("Media/poster.jpg")).unwrap(),
        b"poster"
    );
}

#[test]
fn alias_artwork_reference_from_another_directory_keeps_the_source() {
    use store::library_write::{FilePublication, WriteScope};

    let fx = fixture_with_alias(true);
    let src = fx.a.join("Media");
    let moved = episode(&fx, &src, 1);
    let poster = src.join("poster.jpg");
    std::fs::write(&poster, b"shared poster").unwrap();
    let mut other = episode(&fx, &fx.a.join("Other"), 2);
    other.poster_path = Some(poster.clone());
    fx.store
        .publish_files(
            &WriteScope {
                library_id: other.library_id,
                config_revision: fx
                    .store
                    .library_config_revision(&other.library_id.to_string())
                    .unwrap(),
            },
            &[FilePublication {
                file: &other,
                units: &[domain::UnitKey::new(1, 2)],
                expected_version: Some(&other.version),
                nfo_observation: None,
                probe_required: false,
            }],
            1,
        )
        .unwrap();
    begin(&fx, &moved, &fx.b.join("Media/S01E01.mkv"));
    assert_eq!(fx.store.resume_pending_file_renames(2).unwrap(), 1);
    assert_eq!(std::fs::read(&poster).unwrap(), b"shared poster");
    let remaining = fx
        .store
        .file_coverage_by_id(other.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(remaining.file.poster_path, Some(poster));
    assert_eq!(
        std::fs::read(fx.b.join("Media/poster.jpg")).unwrap(),
        b"shared poster"
    );
}

#[test]
fn same_directory_rename_leaves_directory_artwork_alone() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"poster").unwrap();
    let req = begin(&fx, &file, &src.join("Show S01E01.mkv"));
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| store::library_write::execute_rename_disk(&r))
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
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
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| store::library_write::execute_rename_disk(&r))
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
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
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| store::library_write::execute_rename_disk(&r))
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
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
    fx.store
        .load_file_rename(&req.rename_id)
        .and_then(|r| store::library_write::execute_rename_disk(&r))
        .and_then(|rc| fx.store.record_rename_disk(&rc, 2))
        .unwrap();
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

fn execute(fx: &Fx, rename_id: &str) -> store::library_write::RenameDiskReceipt {
    let request = fx.store.load_file_rename(rename_id).unwrap();
    store::library_write::execute_rename_disk(&request).unwrap()
}

#[test]
fn competitor_appearing_after_observation_is_never_overwritten() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"source").unwrap();
    let dest = fx.b.join("Show");
    let req = begin(&fx, &file, &dest.join("S01E01.mkv"));
    let receipt = execute(&fx, &req.rename_id);
    let competitor = dest.join("poster.jpg");
    fx.store
        .record_rename_disk_observed(&receipt, 2, || {
            std::fs::write(&competitor, b"competitor").unwrap();
        })
        .unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    let destination_bytes = std::fs::read(&competitor).unwrap();
    let retained_source_bytes = std::fs::read(src.join("poster.jpg")).unwrap();
    let pending_deletion_paths = fx.store.list_pending_file_deletions().unwrap();
    assert_eq!(destination_bytes, b"competitor");
    assert_eq!(retained_source_bytes, b"source");
    assert!(
        !pending_deletion_paths
            .iter()
            .any(|p| p.ends_with("a/Show/poster.jpg")),
        "{pending_deletion_paths:?}"
    );
}

#[test]
fn unreadable_destination_is_unknown_never_identical() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"source").unwrap();
    let dest = fx.b.join("Show");
    // A directory at the target: stat succeeds, reading fails.
    std::fs::create_dir_all(dest.join("poster.jpg")).unwrap();
    let req = begin(&fx, &file, &dest.join("S01E01.mkv"));
    let receipt = execute(&fx, &req.rename_id);
    assert!(fx.store.record_rename_disk(&receipt, 2).is_err());
    assert_eq!(std::fs::read(src.join("poster.jpg")).unwrap(), b"source");
    assert_eq!(
        fx.store.load_file_rename(&req.rename_id).unwrap().phase,
        "planned",
        "an unknown observation keeps the last durable decision"
    );
    assert!(fx.store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn identical_destination_clears_sole_owned_source_and_keeps_references_consistent() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("fanart.jpg"), b"same").unwrap();
    let dest = fx.b.join("Show");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("fanart.jpg"), b"same").unwrap();
    let req = begin(&fx, &file, &dest.join("S01E01.mkv"));
    let receipt = execute(&fx, &req.rename_id);
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    let decided = fx.store.load_file_rename(&req.rename_id).unwrap();
    assert_eq!(
        decided.sidecars[0].action,
        store::library_write::SidecarAction::AlreadyIdentical
    );
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
    let queued = fx.store.list_pending_file_deletions().unwrap();
    assert!(
        queued.iter().any(|p| p.ends_with("a/Show/fanart.jpg")),
        "{queued:?}"
    );
}

#[test]
fn failed_rename_waits_for_its_retry_time_in_seconds() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    std::fs::write(src.join("poster.jpg"), b"source").unwrap();
    let dest = fx.b.join("Show");
    std::fs::create_dir_all(dest.join("poster.jpg")).unwrap();
    let req = begin(&fx, &file, &dest.join("S01E01.mkv"));
    assert_eq!(fx.store.resume_pending_file_renames(1_000).unwrap(), 0);
    // Back-off is 60 s: not due one second later, due a minute later.
    assert!(fx.store.list_due_file_rename_ids(1_001).unwrap().is_empty());
    assert_eq!(
        fx.store.list_due_file_rename_ids(1_060).unwrap(),
        vec![req.rename_id.clone()]
    );
}

#[test]
fn failed_leftover_enqueue_keeps_rename_resumable_until_all_sidecars_are_queued() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    let dest = fx.b.join("Show");
    std::fs::create_dir_all(&dest).unwrap();
    for name in ["poster.jpg", "fanart.jpg"] {
        std::fs::write(src.join(name), b"same").unwrap();
        std::fs::write(dest.join(name), b"same").unwrap();
    }
    let req = begin(&fx, &file, &dest.join("S01E01.mkv"));
    let receipt = execute(&fx, &req.rename_id);
    fx.store.record_rename_disk(&receipt, 2).unwrap();
    let db = rusqlite::Connection::open(fx._tmp.path().join("data/library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_leftover_enqueue BEFORE INSERT ON pending_file_deletions
         WHEN NEW.path LIKE '%fanart.jpg'
         BEGIN SELECT RAISE(ABORT, 'injected sidecar enqueue failure'); END;",
    )
    .unwrap();
    let result = fx.store.complete_file_rename(&req.rename_id, 3);
    assert!(
        result.is_err(),
        "failed durable enqueue was acknowledged: {result:?}"
    );
    let pending = fx.store.load_file_rename(&req.rename_id).unwrap();
    assert_eq!(pending.phase, "references_applied");
    assert!(src.join("poster.jpg").is_file());
    assert!(src.join("fanart.jpg").is_file());
    assert!(fx.store.list_due_file_rename_ids(3).unwrap().is_empty());

    db.execute_batch("DROP TRIGGER fail_leftover_enqueue")
        .unwrap();
    assert_eq!(fx.store.resume_pending_file_renames(100).unwrap(), 1);
    assert!(fx.store.load_file_rename(&req.rename_id).is_err());
    let queued = fx.store.list_pending_file_deletions().unwrap();
    for name in ["poster.jpg", "fanart.jpg"] {
        assert_eq!(
            queued
                .iter()
                .filter(|path| **path == src.join(name).to_string_lossy())
                .count(),
            1
        );
    }
    let moved = fx.store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    assert_eq!(moved.file.path, dest.join("S01E01.mkv"));
}

#[test]
fn synchronous_rename_failure_becomes_due_in_unix_seconds() {
    let fx = fixture();
    let src = fx.a.join("Show");
    let file = episode(&fx, &src, 1);
    let dest = fx.a.join("Renamed Show");
    std::fs::create_dir_all(&dest).unwrap();
    for directory in [&src, &dest] {
        std::fs::write(directory.join("fanart.jpg"), b"same").unwrap();
    }
    let db = rusqlite::Connection::open(fx._tmp.path().join("data/library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_sync_leftover BEFORE INSERT ON pending_file_deletions
         BEGIN SELECT RAISE(ABORT, 'injected sidecar enqueue failure'); END;",
    )
    .unwrap();
    let result = fx.store.rename_file_within_root(
        file.file_id,
        &file.version,
        &file.path,
        &dest.join("S01E01.mkv"),
    );
    assert!(result.is_err(), "{result:?}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(fx.store.list_due_file_rename_ids(now).unwrap().is_empty());
    assert_eq!(
        fx.store.list_due_file_rename_ids(now + 120).unwrap().len(),
        1
    );
    db.execute_batch("DROP TRIGGER fail_sync_leftover").unwrap();
    assert_eq!(fx.store.resume_pending_file_renames(now + 120).unwrap(), 1);
    assert_eq!(fx.store.list_pending_file_deletions().unwrap().len(), 1);
}
