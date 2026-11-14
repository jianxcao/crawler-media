//! Scoped scanning publication and ownership verification.
//!
//! Filters walked paths so that only those strictly owned by the requested Library
//! (under the same MediaKind) are admitted, and validates that the Library
//! configuration and state have not changed before publishing.

use std::path::{Path, PathBuf};

use store::{LibraryPathScope, Store, StrictOwner};

mod ownership;
use ownership::relevant_snapshot;

use crate::library_presence::PresenceError;
pub fn walk_video_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            tracing::debug!(path = %path.display(), "Video 扫描跳过软链接");
            continue;
        }
        if meta.is_dir() {
            walk_video_files(&path, out);
        } else if meta.is_file() && library::is_video_file(&path) {
            out.push(path);
        }
    }
}

use crate::management::ApiState;

pub struct ScanScope {
    pub library_id: String,
    pub kind: domain::MediaKind,
    scope: LibraryPathScope,
    resolved_roots: Vec<PathBuf>,
    /// 捕获时的 Library scope 指纹：发布前用它复核持久化 signature（P2-4）。
    signature: String,
}

impl ScanScope {
    /// Capture and resolve the strict scope for a library outside the Store lock.
    pub fn capture(state: &ApiState, library_id: &str) -> Result<Self, PresenceError> {
        let (snapshot, kind, signature) = {
            let store_arc = state.store();
            let guard = &store_arc;
            let snapshot = guard.library_ownership()?;
            let library = snapshot.library(library_id).ok_or(PresenceError::Missing)?;
            let kind = library.kind;
            let signature = crate::scan_operations::scan_scope_signature(library);
            if !snapshot.accepts_work(library_id) {
                return Err(PresenceError::Deleting);
            }
            (snapshot, kind, signature)
        };

        let resolved_roots = snapshot
            .library(library_id)
            .ok_or(PresenceError::Missing)?
            .root_paths
            .iter()
            .map(|root| LibraryPathScope::resolve_path(root))
            .collect::<Result<Vec<_>, _>>()?;
        let relevant = relevant_snapshot(&snapshot, library_id, kind, &resolved_roots)?;
        let scope = LibraryPathScope::resolve(relevant)?;
        if !scope.accepts_work(library_id) {
            return Err(PresenceError::Deleting);
        }

        Ok(Self {
            library_id: library_id.to_string(),
            kind,
            scope,
            resolved_roots,
            signature,
        })
    }

    /// 捕获 scope 的指纹（与持久化的 `scope_signature` 同一编码）。
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// Re-check the captured scope against the persisted Library configuration
    /// before publishing. A Library that stopped accepting work (removal in
    /// progress) or that moved a root invalidates the scan.
    pub fn validate(&self, store: &Store) -> Result<(), PresenceError> {
        let current = store.library_ownership()?;
        if !current.accepts_work(&self.library_id) {
            return Err(PresenceError::Deleting);
        }
        let relevant =
            relevant_snapshot(&current, &self.library_id, self.kind, &self.resolved_roots)?;
        if !self.scope.matches(&relevant) {
            return Err(PresenceError::ScopeChanged);
        }
        Ok(())
    }

    /// Filter paths to retain only those strictly owned by this library.
    pub fn filter_paths(&self, paths: Vec<PathBuf>) -> Result<Vec<PathBuf>, PresenceError> {
        let mut admitted = Vec::with_capacity(paths.len());
        for p in paths {
            let Ok(resolved) = LibraryPathScope::resolve_path(&p) else {
                continue;
            };
            match self.scope.owner(&resolved, self.kind) {
                StrictOwner::Owned(lib) if lib.id == self.library_id => {
                    admitted.push(p);
                }
                StrictOwner::Ambiguous => return Err(PresenceError::Ambiguous),
                _ => {}
            }
        }
        Ok(admitted)
    }

    /// Filter transferred files to retain only those strictly owned by this library.
    pub fn filter_transferred(
        &self,
        files: Vec<library::TransferredFile>,
    ) -> Result<Vec<library::TransferredFile>, PresenceError> {
        let mut admitted = Vec::with_capacity(files.len());
        for file in files {
            let Ok(resolved) = LibraryPathScope::resolve_path(&file.path) else {
                continue;
            };
            match self.scope.owner(&resolved, self.kind) {
                StrictOwner::Owned(lib) if lib.id == self.library_id => {
                    admitted.push(file);
                }
                StrictOwner::Ambiguous => return Err(PresenceError::Ambiguous),
                _ => {}
            }
        }
        Ok(admitted)
    }
}
