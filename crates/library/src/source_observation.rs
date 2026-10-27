//! The single source-content token encoder.
//!
//! Every consumer (Store CAS, Probe, fingerprints, subtitles, Playback) compares
//! against [`current_source_version`]. The token describes *content identity*,
//! never the path: the path is a locator, so rename / re-parent keep the token.
//!
//! * `.strm`: `strm:<sha256 of the exact target bytes>` — the URL is the content.
//! * other files: `file:<size>:<mtime_ns>:<dev>:<ino>` — a same-filesystem
//!   rename keeps every field; a cross-filesystem move is a copy with a new
//!   inode and is conservatively a new source (no byte-identity proof).
//! * unreadable: `missing`.
//!
//! Built from one `stat` (plus one read for `.strm`); no hashing for regular files.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::LibraryError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceObservation {
    pub source_version: String,
    pub size_bytes: Option<u64>,
    pub modified_ns: Option<i64>,
    pub created_ns: Option<i64>,
    pub content_digest: Option<String>,
}

/// Token for an unreadable source; never equals a real observation.
pub const MISSING_SOURCE_VERSION: &str = "missing";

fn is_strm(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
}

fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata.mtime_nsec() + metadata.mtime() * 1_000_000_000
}

fn encode(path: &Path, metadata: &fs::Metadata) -> Result<(String, Option<String>), LibraryError> {
    if is_strm(path) {
        let digest = format!("{:x}", Sha256::digest(fs::read(path)?));
        let version = format!("strm:{digest}");
        return Ok((version, Some(digest)));
    }
    Ok((
        format!(
            "file:{}:{}:{}:{}",
            metadata.len(),
            modified_ns(metadata),
            metadata.dev(),
            metadata.ino()
        ),
        None,
    ))
}

/// The production content token for `path` (see module docs).
pub fn current_source_version(path: &Path) -> String {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| encode(path, &metadata).ok())
        .map(|(version, _)| version)
        .unwrap_or_else(|| MISSING_SOURCE_VERSION.to_owned())
}

pub fn observe_source(path: &Path) -> Result<SourceObservation, LibraryError> {
    let metadata = fs::metadata(path)?;
    let (source_version, content_digest) = encode(path, &metadata)?;
    Ok(SourceObservation {
        source_version,
        size_bytes: Some(metadata.len()),
        modified_ns: Some(modified_ns(&metadata)),
        created_ns: metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok()),
        content_digest,
    })
}
