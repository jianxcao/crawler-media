use std::path::PathBuf;

use domain::MediaKind;
use store::{LibraryOwnership, LibraryPathScope};

use crate::library_presence::PresenceError;

/// Keep every same-kind root that can change ownership inside the scanned
/// Library. Canonicalization must run outside the Store lock: two lexically
/// disjoint roots can refer to the same path through a symlink.
pub(super) fn relevant_snapshot(
    snapshot: &LibraryOwnership,
    library_id: &str,
    kind: MediaKind,
    roots: &[PathBuf],
) -> Result<LibraryOwnership, PresenceError> {
    let mut libraries = Vec::new();
    let mut blocked = std::collections::HashSet::new();
    for library in snapshot
        .libraries()
        .iter()
        .filter(|library| library.kind == kind)
    {
        let mut overlaps = library.id == library_id;
        for root in &library.root_paths {
            let resolved = LibraryPathScope::resolve_path(root)?;
            overlaps |= roots.iter().any(|scan_root| {
                resolved.starts_with(scan_root) || scan_root.starts_with(&resolved)
            });
        }
        if overlaps {
            if !snapshot.accepts_work(&library.id) {
                blocked.insert(library.id.clone());
            }
            libraries.push(library.clone());
        }
    }
    Ok(LibraryOwnership::from_parts(libraries, blocked))
}
