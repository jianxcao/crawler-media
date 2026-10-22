//! P02: rename / re-parent keeps the content token. The token every Probe,
//! fingerprint, subtitle and Playback check consumes (`current_source_version`)
//! must not depend on the path; changed bytes at the destination must advance
//! the FileVersion and invalidate derived facts.
use std::path::{Path, PathBuf};

use api::fingerprint_job::{current_source_version, observe_source};
use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    _guard: tempfile::TempDir,
    store: Store,
    a: PathBuf,
    b: PathBuf,
    other: PathBuf,
}

fn fixture() -> Fx {
    let guard = tempfile::tempdir().unwrap();
    let a = guard.path().join("a");
    let b = guard.path().join("b");
    let other = guard.path().join("other");
    for dir in [&a, &b, &other] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let store = Store::open(guard.path().join("data")).unwrap();
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
    Fx {
        _guard: guard,
        store,
        a,
        b,
        other,
    }
}

/// Publish `path` and bind its real production token, then store probed tracks.
fn published(fx: &Fx, path: &Path, body: &[u8]) -> domain::LibraryFile {
    std::fs::write(path, body).unwrap();
    let media = MediaId::new();
    fx.store
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
    let file = publish_fixture_file(
        &fx.store,
        &FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: fx
            .store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    let version = fx
        .store
        .replace_file_source(
            &scope,
            file.file_id,
            &file.version,
            &observe_source(path).unwrap(),
            1,
        )
        .unwrap();
    assert_eq!(version.source_version, current_source_version(path));
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            codec: Some("h264".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    fx.store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &tracks,
            Some(&version.source_version),
            Some(1000),
        )
        .unwrap();
    fx.store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file
}

fn rename(fx: &Fx, file: &domain::LibraryFile, to: &Path) -> domain::LibraryFile {
    let req = fx
        .store
        .begin_file_rename(file.file_id, &file.version, &file.path, to, 2)
        .unwrap();
    fx.store.apply_file_rename_disk(&req.rename_id, 3).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 4).unwrap();
    fx.store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file
}

fn assert_token_kept(fx: &Fx, before: &domain::LibraryFile, after: &domain::LibraryFile) {
    assert_eq!(
        after.version, before.version,
        "content unchanged: FileVersion kept"
    );
    assert_eq!(
        after.version.source_version,
        current_source_version(&after.path),
        "observed token at the new path must equal the persisted token"
    );
    assert!(
        fx.store
            .get_file_meta(&after.file_id.to_string())
            .unwrap()
            .is_some(),
        "probed details stay readable after a pure rename"
    );
}

#[test]
fn same_root_rename_keeps_observed_token() {
    let fx = fixture();
    let file = published(&fx, &fx.a.join("M.2000.mkv"), b"video");
    let after = rename(&fx, &file, &fx.a.join("Renamed.2000.mkv"));
    assert_token_kept(&fx, &file, &after);
}

#[test]
fn cross_root_and_cross_library_rename_keep_observed_token() {
    let fx = fixture();
    let file = published(&fx, &fx.a.join("M.2000.mkv"), b"video");
    let moved = rename(&fx, &file, &fx.b.join("M.2000.mkv"));
    assert_token_kept(&fx, &file, &moved);
    let reparented = rename(&fx, &moved, &fx.other.join("M.2000.mkv"));
    assert_token_kept(&fx, &file, &reparented);
}

#[test]
fn strm_rename_keeps_token_and_target_change_moves_it() {
    let fx = fixture();
    let path = fx.a.join("M.2000.strm");
    let file = published(&fx, &path, b"https://a/video");
    let after = rename(&fx, &file, &fx.b.join("N.2000.strm"));
    assert_token_kept(&fx, &file, &after);
    std::fs::write(&after.path, b"https://b/video").unwrap();
    assert_ne!(
        current_source_version(&after.path),
        after.version.source_version
    );
}

#[test]
fn destination_with_new_identity_advances_version_and_invalidates() {
    let fx = fixture();
    let file = published(&fx, &fx.a.join("M.2000.mkv"), b"video");
    let to = fx.b.join("M.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(file.file_id, &file.version, &file.path, &to, 2)
        .unwrap();
    fx.store.apply_file_rename_disk(&req.rename_id, 3).unwrap();
    // Cross-filesystem move is a copy: new inode (and here new bytes). Without
    // byte-identity evidence this must be treated as a new source.
    let copy = fx.b.join("copy.tmp");
    std::fs::write(&copy, b"other video").unwrap();
    std::fs::rename(&copy, &to).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 4).unwrap();
    let after = fx
        .store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file;
    assert_eq!(after.path, to);
    assert_eq!(after.version.revision, file.version.revision + 1);
    assert_eq!(after.version.source_version, current_source_version(&to));
    assert!(
        fx.store
            .get_file_meta(&file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert!(
        fx.store
            .scan_probe_obligations(10)
            .unwrap()
            .iter()
            .any(|o| o.ledger_id == file.file_id),
        "new content must be re-probed"
    );
}

#[test]
fn single_encoder_is_shared_with_library_observer() {
    let fx = fixture();
    let path = fx.a.join("x.mkv");
    std::fs::write(&path, b"x").unwrap();
    assert_eq!(
        current_source_version(&path),
        library::observe_source(&path).unwrap().source_version
    );
    assert_eq!(
        current_source_version(&path),
        library::current_source_version(&path)
    );
}

#[test]
fn crash_after_disk_stage_resumes_with_kept_token() {
    let fx = fixture();
    let file = published(&fx, &fx.a.join("M.2000.mkv"), b"video");
    let to = fx.other.join("M.2000.mkv");
    let req = fx
        .store
        .begin_file_rename(file.file_id, &file.version, &file.path, &to, 2)
        .unwrap();
    store::library_write::move_rename_bytes(&req).unwrap();
    // Crash: the ledger still points at `from`; restart drives the intent.
    assert_eq!(fx.store.resume_pending_file_renames(3).unwrap(), 1);
    let after = fx
        .store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file;
    assert_eq!(after.path, to);
    assert_token_kept(&fx, &file, &after);
    // Idempotent: nothing left to resume, no further revision.
    assert_eq!(fx.store.resume_pending_file_renames(4).unwrap(), 0);
    assert_eq!(
        fx.store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .version,
        file.version
    );
}
