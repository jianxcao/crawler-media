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

pub fn observe_source(path: &Path) -> Result<SourceObservation, LibraryError> {
    let metadata = fs::metadata(path)?;
    let size_bytes = Some(metadata.len());
    let modified_ns = Some(metadata.mtime_nsec() + metadata.mtime() * 1_000_000_000);
    let created_ns = None;

    let is_strm = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("strm"))
        .unwrap_or(false);

    let (content_digest, source_version) = if is_strm {
        let content = fs::read_to_string(path).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(content.trim().as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        let version = format!("strm:{digest}");
        (Some(digest), version)
    } else {
        let version = format!(
            "stat:{}:{}",
            metadata.len(),
            metadata.mtime_nsec() + metadata.mtime() * 1_000_000_000
        );
        (None, version)
    };

    Ok(SourceObservation {
        source_version,
        size_bytes,
        modified_ns,
        created_ns,
        content_digest,
    })
}
