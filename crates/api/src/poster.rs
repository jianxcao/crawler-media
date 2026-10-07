use std::path::{Path, PathBuf};

pub fn poster_beside(path: &str) -> Option<PathBuf> {
    let poster = Path::new(path).parent()?.join("poster.jpg");
    poster.is_file().then_some(poster)
}
