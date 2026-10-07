use domain::MediaKind;
use store::Store;

#[test]
fn strict_owner_resolves_missing_descendants_without_parent_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let store = Store::open(&tmp.path().join("data")).unwrap();
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
    let store = Store::open(&tmp.path().join("data")).unwrap();
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
