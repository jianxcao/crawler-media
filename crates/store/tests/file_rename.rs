//! N03: a recorded file can only be renamed within its own root/Library, with
//! lifecycle, Media and target revalidated in the Store transaction.
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
    file: domain::LibraryFile,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(a.join("nested")).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    for (name, root) in [("A", a.clone()), ("B", b.clone()), ("C", a.join("nested"))] {
        store
            .create_library(
                MediaKind::Movie,
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
        _tmp: tmp,
        store,
        a,
        b,
        media,
        file,
    }
}

fn rename(fx: &Fx, to: &Path) -> Result<(), store::StoreError> {
    fx.store
        .rename_file_within_root(fx.file.file_id, &fx.file.version, &fx.file.path, to)
}

fn recorded_path(fx: &Fx) -> PathBuf {
    fx.store
        .file_coverage_by_id(fx.file.file_id)
        .unwrap()
        .unwrap()
        .file
        .path
}

#[test]
fn rename_rejects_cross_root_nested_library_and_symlink_escape() {
    let fx = fixture();
    let outside = fx._tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, fx.a.join("link")).unwrap();
    for to in [
        fx.b.join("M.mkv"),
        fx.a.join("nested/M.mkv"),
        fx.a.join("link/M.mkv"),
        fx.a.join("../b/M.mkv"),
    ] {
        assert!(
            rename(&fx, &to).is_err(),
            "{} must be rejected",
            to.display()
        );
        assert_eq!(recorded_path(&fx), fx.file.path);
    }
}

#[test]
fn rename_rejects_frozen_media_stale_version_and_occupied_target() {
    let fx = fixture();
    let other = fx.a.join("Other.mkv");
    std::fs::write(&other, b"o").unwrap();
    publish_fixture_file(
        &fx.store,
        &FixtureFile {
            media_id: fx.media,
            path: other.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(rename(&fx, &other).is_err());
    let mut stale = fx.file.version.clone();
    stale.revision += 7;
    assert!(
        fx.store
            .rename_file_within_root(fx.file.file_id, &stale, &fx.file.path, &fx.a.join("x.mkv"))
            .is_err()
    );
    store::test_support::set_media_identity_state(&fx.store, fx.media, "merging").unwrap();
    assert!(rename(&fx, &fx.a.join("y.mkv")).is_err());
    assert_eq!(recorded_path(&fx), fx.file.path);
}

#[test]
fn rename_within_root_keeps_identity() {
    let fx = fixture();
    let to = fx.a.join("Sub/M (2000).mkv");
    rename(&fx, &to).unwrap();
    assert_eq!(recorded_path(&fx), to);
    let coverage = fx
        .store
        .file_coverage_by_path(to.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(coverage.file.file_id, fx.file.file_id);
    assert_eq!(coverage.file.library_id, fx.file.library_id);
}
