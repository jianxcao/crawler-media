use std::{fs, path::Path};

use library::sidecar_cleanup::prune_after_delete;

fn write_files(dir: &Path, names: &[&str]) {
    fs::create_dir_all(dir).unwrap();
    for name in names {
        fs::write(dir.join(name), name.as_bytes()).unwrap();
    }
}

#[test]
fn deleting_shorter_stem_preserves_longer_video_sidecars() {
    for sibling in ["Film-extended", "Film.part2"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let dir = root.join("movie");
        write_files(
            &dir,
            &["Film.mkv", "Film.nfo", "Film.zh.srt", "Film-thumb.jpg"],
        );
        let sibling_names = [
            format!("{sibling}.mkv"),
            format!("{sibling}.nfo"),
            format!("{sibling}.zh.srt"),
            format!("{sibling}-thumb.jpg"),
        ];
        for name in &sibling_names {
            fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        let deleted = dir.join("Film.mkv");
        fs::remove_file(&deleted).unwrap();

        prune_after_delete(&deleted, root);

        for name in &sibling_names {
            assert_eq!(
                fs::read(dir.join(name)).unwrap_or_else(|error| panic!("{name}: {error}")),
                name.as_bytes(),
            );
        }
        for name in ["Film.nfo", "Film.zh.srt", "Film-thumb.jpg"] {
            assert!(!dir.join(name).exists(), "{name} should be removed");
        }
    }
}

#[test]
fn deleting_one_container_preserves_same_stem_shared_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("movie");
    let retained = ["Film.mp4", "Film.nfo", "Film.zh.srt", "Film-thumb.jpg"];
    write_files(&dir, &retained);
    fs::write(dir.join("Film.mkv"), b"deleted").unwrap();
    let deleted = dir.join("Film.mkv");
    fs::remove_file(&deleted).unwrap();

    let removed = prune_after_delete(&deleted, temp.path());

    assert!(removed.is_empty());
    for name in retained {
        assert_eq!(fs::read(dir.join(name)).unwrap(), name.as_bytes());
    }
}

#[test]
fn deleting_longer_stem_removes_only_its_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("movie");
    let retained = ["Film.mkv", "Film.nfo", "Film.zh.srt", "Film-thumb.jpg"];
    let deleted_sidecars = [
        "Film-extended.nfo",
        "Film-extended.zh.srt",
        "Film-extended-thumb.jpg",
    ];
    write_files(&dir, &retained);
    write_files(&dir, &deleted_sidecars);
    let deleted = dir.join("Film-extended.mkv");
    fs::write(&deleted, b"deleted").unwrap();
    fs::remove_file(&deleted).unwrap();

    prune_after_delete(&deleted, temp.path());

    for name in retained {
        assert_eq!(fs::read(dir.join(name)).unwrap(), name.as_bytes());
    }
    for name in deleted_sidecars {
        assert!(!dir.join(name).exists(), "{name} should be removed");
    }
}

#[test]
fn deleting_last_video_removes_sidecars_and_empty_ancestors_before_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let work = root.join("Work");
    let season = work.join("Season 01");
    write_files(&season, &["Episode.strm", "Episode.nfo", "Episode.zh.srt"]);
    write_files(&work, &["tvshow.nfo", "poster.jpg"]);
    let deleted = season.join("Episode.strm");
    fs::remove_file(&deleted).unwrap();

    prune_after_delete(&deleted, &root);

    assert!(!work.exists());
    assert!(root.is_dir());
}

#[cfg(unix)]
#[test]
fn unreadable_directory_preserves_all_sidecars() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("movie");
    let names = ["Film.nfo", "Film.zh.srt", "Film-thumb.jpg"];
    write_files(&dir, &names);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&dir).is_ok() {
        // A privileged test process may bypass directory permissions.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }

    let removed = prune_after_delete(&dir.join("Film.mkv"), temp.path());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(removed.is_empty());
    for name in names {
        assert_eq!(fs::read(dir.join(name)).unwrap(), name.as_bytes());
    }
}
