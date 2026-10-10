use std::path::{Path, PathBuf};

use downloader::{PathMap, apply_maps};

#[test]
fn sibling_download_directory_is_not_rerooted() {
    let maps = [PathMap::new("/downloads", "/host/qb")];
    assert_eq!(
        apply_maps(Path::new("/downloads-old/Film.mkv"), &maps),
        PathBuf::from("/downloads-old/Film.mkv")
    );
}

#[test]
fn exact_download_root_maps_to_exact_host_root() {
    let maps = [PathMap::new("/downloads", "/host/qb")];
    assert_eq!(
        apply_maps(Path::new("/downloads"), &maps),
        PathBuf::from("/host/qb")
    );
}

#[test]
fn trailing_separators_do_not_change_root_or_child_matching() {
    let map = PathMap::new("/downloads/", "/host/qb/");
    assert_eq!(map.remap(Path::new("/downloads")), Some("/host/qb".into()));
    assert_eq!(
        map.remap(Path::new("/downloads/Film.mkv")),
        Some("/host/qb/Film.mkv".into())
    );
}

#[test]
fn nested_maps_keep_configured_first_match_not_longest_prefix() {
    let broad_first = [
        PathMap::new("/downloads", "/host/qb"),
        PathMap::new("/downloads/nested", "/host/other"),
    ];
    let path = Path::new("/downloads/nested/Film.mkv");
    assert_eq!(
        apply_maps(path, &broad_first),
        PathBuf::from("/host/qb/nested/Film.mkv")
    );
    let narrow_first = [broad_first[1].clone(), broad_first[0].clone()];
    assert_eq!(
        apply_maps(path, &narrow_first),
        PathBuf::from("/host/other/Film.mkv")
    );
}

#[test]
fn sibling_roots_choose_only_the_component_matching_map() {
    let maps = [
        PathMap::new("/downloads", "/host/qb"),
        PathMap::new("/downloads-other", "/host/other"),
    ];
    assert_eq!(
        apply_maps(Path::new("/downloads-other/Film.mkv"), &maps),
        PathBuf::from("/host/other/Film.mkv")
    );
}
