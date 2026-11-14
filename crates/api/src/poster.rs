use std::path::{Path, PathBuf};

use crate::file_stat_read::FileStatRead;

/// `poster.jpg` beside the video file, existence checked through the browse seam.
pub fn poster_beside(path: &str, files: &dyn FileStatRead) -> Option<PathBuf> {
    let poster = Path::new(path).parent()?.join("poster.jpg");
    files.is_file(&poster).then_some(poster)
}
