//! Request-local Library ownership snapshot plus the one rule that classifies a
//! NEW file at admission time: a pure longest-root-prefix decision per path (no
//! IO, no stat, no canonicalize).
//!
//! A published file's owner is never decided here: it is the recorded
//! `library_files.library_id`, which survives a root rename or removal.

use std::collections::HashSet;
use std::path::Path;

use domain::MediaKind;

use super::Library;

/// Outcome of the pure longest-root match for one path.
enum OwnerMatch<'a> {
    Unique(&'a Library),
    Ambiguous,
    Unmatched,
}

/// Longest-root-prefix match, same rule as the historical `library_for_path`:
/// same kind only, the largest matching root component count wins, an equal
/// depth reached by two different libraries is ambiguous, and a path that
/// matches no root is `Unmatched`. `Path::starts_with` compares whole
/// components, so `/owned-other` is not owned by a `/owned` root.
fn match_owner<'a>(libraries: &'a [Library], path: &Path, kind: MediaKind) -> OwnerMatch<'a> {
    let mut best: Option<(usize, &'a Library)> = None;
    let mut ambiguous = false;
    for library in libraries.iter().filter(|library| library.kind == kind) {
        let Some(depth) = library
            .root_paths
            .iter()
            .filter(|root| path.starts_with(root))
            .map(|root| root.components().count())
            .max()
        else {
            continue;
        };
        match best {
            Some((best_depth, _)) if best_depth > depth => {}
            Some((best_depth, best_library)) if best_depth == depth => {
                if best_library.id != library.id {
                    ambiguous = true;
                }
            }
            _ => {
                best = Some((depth, library));
                ambiguous = false;
            }
        }
    }
    if ambiguous {
        OwnerMatch::Ambiguous
    } else if let Some((_, library)) = best {
        OwnerMatch::Unique(library)
    } else {
        OwnerMatch::Unmatched
    }
}

/// The kind's default library, in the snapshot's `(sort_order, name)` order.
fn default_for(libraries: &[Library], kind: MediaKind) -> Option<&Library> {
    libraries
        .iter()
        .find(|library| library.kind == kind && library.is_default)
}

/// Classify a NEW file (a scan result, a transfer destination, a claim) into the
/// Library that owns its path. Nothing matched means "no Library": there is
/// deliberately no default-Library fallback, because a default would hand a file
/// that no root covers the default Library's ACL. Ambiguity stays unresolved.
pub fn classify_new_file<'a>(
    libraries: &'a [Library],
    path: &Path,
    kind: MediaKind,
) -> Option<&'a Library> {
    match match_owner(libraries, path, kind) {
        OwnerMatch::Unique(library) => Some(library),
        OwnerMatch::Ambiguous | OwnerMatch::Unmatched => None,
    }
}

/// A request-local view of Library configuration, roots, and blocked ids.
#[derive(Clone, Debug)]
pub struct LibraryOwnership {
    libraries: Vec<Library>,
    blocked: HashSet<String>,
}

impl LibraryOwnership {
    pub fn from_parts(libraries: Vec<Library>, blocked: HashSet<String>) -> Self {
        Self { libraries, blocked }
    }

    pub fn libraries(&self) -> &[Library] {
        &self.libraries
    }

    pub fn library(&self, id: &str) -> Option<&Library> {
        self.libraries.iter().find(|library| library.id == id)
    }

    pub fn default_for(&self, kind: MediaKind) -> Option<&Library> {
        default_for(&self.libraries, kind)
    }

    /// Same as `Store::library_accepts_work`: an existing library with no
    /// `library_removals` row accepts work. The removal row blocks regardless
    /// of state, including `succeeded`.
    pub fn accepts_work(&self, id: &str) -> bool {
        self.library(id).is_some() && !self.blocked.contains(id)
    }
}
