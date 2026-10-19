//! Artwork that exists beside a library file.
//!
//! The same layout the artwork routes serve: `poster.jpg` beside the video (or one
//! level up, for a season directory), `fanart.jpg` beside it (or one level up), and
//! the per-episode still naming. Discovery is a pure filesystem observation; the
//! caller persists the result on the file row.

use std::path::{Path, PathBuf};

/// Artwork paths that actually exist beside one video file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveredArtwork {
    pub poster: Option<PathBuf>,
    pub backdrop: Option<PathBuf>,
    pub still: Option<PathBuf>,
}

impl DiscoveredArtwork {
    /// Observe the artwork next to `video`, mirroring the browse/route layout.
    pub fn discover(video: &Path) -> Self {
        let Some(dir) = video.parent() else {
            return Self::default();
        };
        let parent = dir.parent().unwrap_or(dir);
        let poster = [dir.join("poster.jpg"), parent.join("poster.jpg")]
            .into_iter()
            .find(|candidate| candidate.is_file());
        let backdrop = [dir.join("fanart.jpg"), parent.join("fanart.jpg")]
            .into_iter()
            .find(|candidate| candidate.is_file());
        let stem = video
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let still = [
            video.with_file_name(format!("{stem}-thumb.jpg")),
            dir.join(format!("{stem}-still.jpg")),
            dir.join(format!("{stem}.jpg")),
            dir.join("still.jpg"),
        ]
        .into_iter()
        .find(|candidate| candidate.is_file());
        Self {
            poster,
            backdrop,
            still,
        }
    }
}
