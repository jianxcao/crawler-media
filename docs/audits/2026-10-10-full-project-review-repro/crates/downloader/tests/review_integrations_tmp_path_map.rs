use std::path::{Path, PathBuf};
use downloader::{PathMap, apply_maps};

#[test]
fn sibling_download_directory_must_not_be_rerooted() {
    let map = PathMap::new("/downloads", "/host/qb");
    assert_eq!(
        apply_maps(Path::new("/downloads-old/Film.mkv"), &[map]),
        PathBuf::from("/downloads-old/Film.mkv")
    );
}
