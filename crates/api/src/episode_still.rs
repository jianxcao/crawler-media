use std::path::{Path, PathBuf};

/// A still belongs to one episode file, including when a season is stored flat.
pub(crate) fn path(video: &Path) -> PathBuf {
    video.with_file_name(format!(
        "{}-still.jpg",
        video
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
    ))
}

/// Older libraries may have a shared `still.jpg`; keep it readable until refreshed.
pub(crate) fn existing(video: &Path) -> Option<PathBuf> {
    let own = path(video);
    if own.is_file() {
        return Some(own);
    }
    video
        .parent()
        .map(|dir| dir.join("still.jpg"))
        .filter(|p| p.is_file())
}
