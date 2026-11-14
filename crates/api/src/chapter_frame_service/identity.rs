//! Bounded names retain the complete immutable publication identity.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use store::ChapterFrameArtifact;

/// Source names may already consume NAME_MAX. Hash the complete basename rather
/// than truncating identity-bearing suffixes or assuming a character is one byte.
pub fn chapter_frame_artifact_path(
    video: &Path,
    generation: &str,
    index: usize,
    artifact: &ChapterFrameArtifact,
) -> PathBuf {
    let basename = video.file_name().unwrap_or_default().as_encoded_bytes();
    let source = format!("{:x}", Sha256::digest(basename));
    video.with_file_name(format!(
        "{source}-chapter-{generation}-{index}-a{}-{}.jpg",
        artifact.attempt, artifact.id
    ))
}
