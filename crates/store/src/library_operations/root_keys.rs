//! Requested roots prepared once, outside any transaction (C08).
//!
//! A reconfiguration persists both spellings of every requested root: the caller's
//! `display_path` and the canonical `path_key` (symlinks resolved when the directory
//! exists). Every later comparison — coverage, longest-prefix ownership, row lookup,
//! pruning — uses the persisted key on whole path components, so recovery never
//! re-interprets an accepted request through a symlink that changed since.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::StoreError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PreparedRoot {
    pub display_path: PathBuf,
    pub path_key: String,
}

/// Canonicalize the requested roots (filesystem IO: call before the write lock) and
/// refuse two spellings of one directory.
pub(crate) fn prepare_roots(paths: &[PathBuf]) -> Result<Vec<PreparedRoot>, StoreError> {
    let mut prepared: Vec<PreparedRoot> = Vec::with_capacity(paths.len());
    for path in paths {
        let path_key = crate::path_key::canonical_root_key(&path.to_string_lossy())?;
        if prepared.iter().any(|root| root.path_key == path_key) {
            return Err(StoreError::Invalid(format!(
                "root {} duplicates another requested root ({path_key})",
                path.display()
            )));
        }
        prepared.push(PreparedRoot {
            display_path: path.clone(),
            path_key,
        });
    }
    Ok(prepared)
}

/// The requested root owning `key` (longest component-boundary match).
pub(crate) fn longest_root<'a>(key: &str, roots: &'a [PreparedRoot]) -> Option<&'a PreparedRoot> {
    let path = Path::new(key);
    roots
        .iter()
        .filter(|root| path.starts_with(Path::new(&root.path_key)))
        .max_by_key(|root| Path::new(&root.path_key).components().count())
}
