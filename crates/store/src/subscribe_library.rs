use std::path::Path;

use std::collections::{HashMap, HashSet};
use subscribe::{QualityFact, SubscribeFacts};

use super::{StoreError, SubscribeLibrarySnapshot};
use crate::library_scope::{LibraryPathResolver, LibraryPathScope, StrictOwner};

/// Only recorded coverage contributes ownership facts. Unnumbered TV files remain
/// visible in the Library without claiming a whole work or any episode.
fn coverage_coordinates(coverage: &domain::FileCoverage) -> Vec<(Option<u32>, Option<u32>)> {
    coverage
        .units
        .iter()
        .map(|unit| {
            if unit.is_whole() {
                (None, None)
            } else {
                (
                    u32::try_from(unit.season).ok(),
                    u32::try_from(unit.episode).ok(),
                )
            }
        })
        .collect()
}

/// Saved path observations tied to one immutable database snapshot.
/// Construct outside the Store lock; `evaluate` only reads these owned facts.
pub struct ResolvedSubscribeFacts<'a> {
    snapshot: &'a SubscribeLibrarySnapshot,
    target_library_id: String,
    saved: SubscribeFacts,
}

struct LocalPathResolver;
impl LibraryPathResolver for LocalPathResolver {
    fn resolve_path(&self, path: &Path) -> Result<std::path::PathBuf, StoreError> {
        LibraryPathScope::resolve_path(path)
    }
}

pub fn resolve_subscribe_facts(
    snapshot: &SubscribeLibrarySnapshot,
) -> Result<ResolvedSubscribeFacts<'_>, StoreError> {
    resolve_subscribe_facts_with(snapshot, &LocalPathResolver)
}

/// Resolve only unregistered saved destinations; recorded files own their Library.
pub fn resolve_subscribe_facts_with<'a>(
    snapshot: &'a SubscribeLibrarySnapshot,
    resolver: &dyn LibraryPathResolver,
) -> Result<ResolvedSubscribeFacts<'a>, StoreError> {
    let target_library_id = match snapshot.subscribe.library_id {
        Some(id) => id.to_string(),
        None => snapshot
            .ownership
            .default_for(snapshot.kind)
            .map(|lib| lib.id.clone())
            .ok_or_else(|| StoreError::Missing("target Library".into()))?,
    };

    let target_library = snapshot
        .ownership
        .library(&target_library_id)
        .ok_or_else(|| StoreError::Missing("target Library".into()))?;

    if target_library.kind != snapshot.kind {
        return Err(StoreError::Protected(
            "target Library kind does not match Media".into(),
        ));
    }
    let registered: HashMap<_, _> = snapshot
        .rows
        .iter()
        .map(|row| {
            (
                row.file.path.to_string_lossy().into_owned(),
                row.file.library_id.to_string(),
            )
        })
        .collect();
    let needs_resolution = snapshot.saved.entries().any(|(_, fact)| {
        fact.path
            .as_ref()
            .is_some_and(|path| !registered.contains_key(path))
    });
    let scope = needs_resolution
        .then(|| LibraryPathScope::resolve_with(snapshot.ownership.clone(), resolver))
        .transpose()?;
    let mut saved = SubscribeFacts::default();
    for ((season, episode), fact) in snapshot.saved.entries() {
        let Some(path) = fact.path.as_deref() else {
            continue;
        };
        let owned = if let Some(owner) = registered.get(path) {
            owner == &target_library_id
        } else if let Some(scope) = &scope {
            let resolved = resolver.resolve_path(Path::new(path))?;
            matches!(scope.owner(&resolved, snapshot.kind), StrictOwner::Owned(owner) if owner.id == target_library_id)
        } else {
            false
        };
        if owned {
            saved.replace(season, episode, fact.clone());
            if let Some(quality) = snapshot.saved.quality(path) {
                saved.set_quality(path.to_string(), quality.clone());
            }
        } else {
            tracing::debug!(subscribe_id=%snapshot.subscribe.id, library_id=%target_library_id, path,
                "saved Subscribe destination is outside its target Library");
        }
    }
    Ok(ResolvedSubscribeFacts {
        snapshot,
        target_library_id,
        saved,
    })
}

impl ResolvedSubscribeFacts<'_> {
    /// No path canonicalization or Store access; presence is supplied by the caller.
    pub fn evaluate(&self, is_file: &dyn Fn(&Path) -> bool) -> Result<SubscribeFacts, StoreError> {
        let snapshot = self.snapshot;
        let mut facts = self.saved.clone();
        let registered: HashSet<_> = snapshot
            .rows
            .iter()
            .map(|row| row.file.path.to_string_lossy().into_owned())
            .collect();
        for coverage in &snapshot.rows {
            // Ownership is the recorded Library, not a root-prefix guess.
            if coverage.file.library_id.to_string() != self.target_library_id {
                continue;
            }
            let path = coverage.file.path.to_string_lossy().into_owned();
            if !is_file(Path::new(&path)) {
                continue;
            }
            let fact = QualityFact {
                score: coverage.file.filter_score.unwrap_or(0),
                path: Some(path.clone()),
            };
            // A physical file contributes each explicitly recorded logical unit.
            let coordinates = coverage_coordinates(coverage);
            for (season, episode) in coordinates {
                let stale = facts
                    .get(season, episode)
                    .and_then(|f| f.path.as_deref())
                    .map(|old| !registered.contains(old))
                    .unwrap_or(false);

                if stale {
                    facts.replace(season, episode, fact.clone());
                } else {
                    facts.upsert(season, episode, fact.clone());
                }
            }

            if let Some(quality_res) = snapshot.qualities.get(&coverage.file.file_id) {
                match quality_res {
                    Ok(quality) => facts.set_quality(path.clone(), quality.clone()),
                    Err(e) => {
                        return Err(StoreError::Invalid(format!(
                            "corrupt persisted quality: {e}"
                        )));
                    }
                }
            }
        }

        let mut scoped = SubscribeFacts::default();
        for ((season, episode), fact) in facts.entries() {
            scoped.replace(season, episode, fact.clone());
            if let Some(path) = fact.path.as_deref()
                && let Some(quality) = facts.quality(path)
            {
                scoped.set_quality(path.to_string(), quality.clone());
            }
        }
        Ok(scoped)
    }
}
