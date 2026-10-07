use std::path::{Path, PathBuf};

/// A save-path prefix mapping: any completed file whose path starts with
/// `from` is re-rooted under `to`. This bridges container downloaders whose
/// reported save path (`/downloads/...`) is not visible on the host running
/// the transfer worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathMap {
    pub from: String,
    pub to: PathBuf,
}

impl PathMap {
    pub fn new(from: impl Into<String>, to: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }

    pub fn remap(&self, path: &Path) -> Option<PathBuf> {
        let raw = path.to_string_lossy();
        let rest = raw.strip_prefix(&self.from)?;
        Some(self.to.join(rest.trim_start_matches('/')))
    }

    /// Convert a host-visible path back to the downloader's path namespace.
    pub fn remap_to_downloader(&self, path: &Path) -> Option<String> {
        let rest = path.strip_prefix(&self.to).ok()?;
        let mut remote = PathBuf::from(&self.from);
        remote.push(rest);
        Some(remote.to_string_lossy().into_owned())
    }
}

/// Apply the first matching mapping; return the path unchanged when none match.
pub fn apply_maps(path: &Path, maps: &[PathMap]) -> PathBuf {
    maps.iter()
        .find_map(|map| map.remap(path))
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_reroots_matching_prefix() {
        let map = PathMap::new("/downloads", "/host/qb");
        let mapped = map.remap(Path::new("/downloads/Show/S01E01.mkv")).unwrap();
        assert_eq!(mapped, PathBuf::from("/host/qb/Show/S01E01.mkv"));
    }

    #[test]
    fn remap_keeps_unmatched_paths() {
        let map = PathMap::new("/downloads", "/host/qb");
        assert!(map.remap(Path::new("/elsewhere/Show.mkv")).is_none());
    }

    #[test]
    fn apply_maps_uses_first_match() {
        let maps = vec![
            PathMap::new("/downloads", "/host/qb"),
            PathMap::new("/downloads", "/ignored"),
        ];
        let out = apply_maps(Path::new("/downloads/a/b.mkv"), &maps);
        assert_eq!(out, PathBuf::from("/host/qb/a/b.mkv"));
    }

    #[test]
    fn apply_maps_passthrough_without_match() {
        let maps = vec![PathMap::new("/downloads", "/host/qb")];
        let out = apply_maps(Path::new("/local/file.mkv"), &maps);
        assert_eq!(out, PathBuf::from("/local/file.mkv"));
    }

    #[test]
    fn remap_to_downloader_translates_host_path_and_rejects_prefix_siblings() {
        let map = PathMap::new("/downloads", "/host/qb");
        assert_eq!(
            map.remap_to_downloader(Path::new("/host/qb/Movies/Dune")),
            Some("/downloads/Movies/Dune".into())
        );
        assert_eq!(
            map.remap_to_downloader(Path::new("/host/qb2/Movies/Dune")),
            None
        );
    }
}
