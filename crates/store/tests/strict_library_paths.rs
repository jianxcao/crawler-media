use domain::MediaKind;
use store::Store;

#[test]
fn strict_owner_resolves_missing_descendants_without_parent_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "test",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let inside = root.join("new/sub/file.mkv");
    assert_eq!(
        store
            .library_for_path_strict(&inside, MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id,
        library.id
    );
    let escape = root.join("new/../../outside/file.mkv");
    assert!(
        store
            .library_for_path_strict(&escape, MediaKind::Movie)
            .unwrap()
            .is_none()
    );
    assert!(!root.join("new").exists());
    assert!(
        store
            .library_for_path_strict(&outside.join("file.mkv"), MediaKind::Movie)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn strict_owner_rejects_symlink_ancestors_and_dangling_links() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("missing"), root.join("dangling")).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "test",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert!(
        store
            .library_for_path_strict(&root.join("link/new/file.mkv"), MediaKind::Movie)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .library_for_path_strict(&root.join("dangling/file.mkv"), MediaKind::Movie)
            .is_err()
    );
}

/// A root is **recorded** with the spelling the operator gave it while matching runs
/// on resolved paths, so the two halves must not be conflated:
/// - a descendant addressed through a different (canonical) spelling still belongs
///   to that Library, and
/// - the returned Library keeps the recorded spelling, because callers compare
///   their own path against `root_paths` (`auto_resolve::has_local_scan_metadata`
///   picks the show root that way).
///
/// The second half is the regression: returning an internally resolved clone made
/// every such caller miss on macOS, where the tempdir is spelled `/var/...` while
/// `canonicalize` answers `/private/var/...`.
#[cfg(unix)]
#[test]
fn alias_spelled_root_keeps_recorded_spelling_and_classifies_canonical_descendants() {
    let tmp = tempfile::tempdir().unwrap();
    let real_root = tmp.path().join("real-root");
    std::fs::create_dir_all(&real_root).unwrap();
    let alias = tmp.path().join("alias-root");
    std::os::unix::fs::symlink(&real_root, &alias).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "alias",
            &[alias.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert_eq!(library.root_paths, vec![alias.clone()]);

    let canonical_child = real_root.join("film.mkv");
    let owner = store
        .library_for_path_strict(&canonical_child, MediaKind::Movie)
        .unwrap()
        .expect("the canonical descendant belongs to the alias-spelled root");
    assert_eq!(owner.id, library.id);
    assert_eq!(
        owner.root_paths,
        vec![alias.clone()],
        "the matched Library must keep the recorded root spelling"
    );

    let alias_child = alias.join("film.mkv");
    assert_eq!(
        store
            .library_for_path_strict(&alias_child, MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id,
        library.id
    );
}

/// Containment between a caller's path and a recorded root must tolerate spelling
/// aliases in both directions, and must not claim containment for a sibling.
#[cfg(unix)]
#[test]
fn path_under_root_compares_resolved_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let real_root = tmp.path().join("real-root");
    std::fs::create_dir_all(real_root.join("season1")).unwrap();
    let alias = tmp.path().join("alias-root");
    std::os::unix::fs::symlink(&real_root, &alias).unwrap();

    // Both directions of the alias, an existing and a not-yet-existing descendant.
    assert!(store::path_under_root(&real_root.join("film.mkv"), &alias));
    assert!(store::path_under_root(&alias.join("film.mkv"), &real_root));
    assert!(store::path_under_root(
        &alias.join("season1/film.mkv"),
        &real_root
    ));
    // A sibling that merely shares the prefix is not contained.
    assert!(!store::path_under_root(
        &tmp.path().join("real-root-other/film.mkv"),
        &alias
    ));
    // The root itself counts.
    assert!(store::path_under_root(&real_root, &alias));
}
