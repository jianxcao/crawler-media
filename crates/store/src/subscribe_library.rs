use std::path::Path;

use domain::{MediaKind, Subscribe};
use subscribe::{QualityFact, SubscribeFacts};

use crate::library_scope::{LibraryPathScope, StrictOwner};
use super::{Store, StoreError};

/// Unit coordinates of one coverage, as the legacy per-unit join rows expressed them:
/// a whole-file unit（整片 / `(-1,-1)`）becomes `(None, None)`. An empty coverage keeps
/// the legacy LEFT JOIN shape of a single `(None, None)` row.
fn coverage_coordinates(coverage: &domain::FileCoverage) -> Vec<(Option<u32>, Option<u32>)> {
    if coverage.units.is_empty() {
        return vec![(None, None)];
    }
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

impl Store {
    /// Coordinate a Subscribe's local facts with the selected shared Library.
    /// In-flight coverage is deliberately absent; recorded user deletion facts
    /// are retained instead of implicitly authorizing another download.
    pub fn load_library_subscribe_facts(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeFacts, StoreError> {
        self.library_subscribe_facts(subscribe, kind)
            .map_err(|error| {
                tracing::error!(subscribe_id = %subscribe.id, library_id = ?subscribe.library_id,
                %error, "加载目标 Library 订阅事实失败");
                error
            })
    }

    fn library_subscribe_facts(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeFacts, StoreError> {
        let snapshot = self.subscribe_library_snapshot(subscribe, kind)?;
        evaluate_subscribe_facts_pure(&snapshot)
    }
}

/// Pure evaluation of SubscribeFacts given a SubscribeLibrarySnapshot.
/// This evaluation is purely database-backed and does NOT perform filesystem I/O.
pub fn evaluate_subscribe_facts_pure(
    snapshot: &super::subscribe_library_snapshot::SubscribeLibrarySnapshot,
) -> Result<SubscribeFacts, StoreError> {
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
    let scope = LibraryPathScope::resolve(snapshot.ownership.clone())?;

    let mut facts = SubscribeFacts::default();

    for ((season, episode), fact) in snapshot.saved.entries() {
        let Some(path) = fact.path.as_deref() else {
            continue;
        };
        let p = Path::new(path);
        let resolved = LibraryPathScope::resolve_path(p)?;
        match scope.owner(&resolved, snapshot.kind) {
            StrictOwner::Owned(owner) if owner.id == target_library.id => {
                facts.replace(season, episode, fact.clone());
                if let Some(quality) = snapshot.saved.quality(path) {
                    facts.set_quality(path.to_string(), quality.clone());
                }
            }
            _ => {
                tracing::info!(
                    subscribe_id = %snapshot.subscribe.id,
                    library_id = %target_library.id,
                    season,
                    episode,
                    path,
                    "已保存事实不属于目标 Library，本次不把它当作已拥有内容"
                );
            }
        }
    }

    for coverage in &snapshot.rows {
        let path = coverage.file.path.to_string_lossy().into_owned();
        let resolved = LibraryPathScope::resolve_path(Path::new(&path))?;
        if !matches!(scope.owner(&resolved, snapshot.kind), StrictOwner::Owned(owner) if owner.id == target_library.id) {
            continue;
        }
        let fact = QualityFact {
            score: coverage.file.filter_score.unwrap_or(0),
            path: Some(path.clone()),
        };
        // 一个物理文件可覆盖多个 unit；legacy 拿到的是逐 unit 的 join 行，这里逐 unit 保值。
        let coordinates = coverage_coordinates(coverage);
        for (season, episode) in coordinates {
            let stale = facts
                .get(season, episode)
                .and_then(|f| f.path.as_deref())
                .map(|old| {
                    !snapshot
                        .rows
                        .iter()
                        .any(|c| c.file.path.to_string_lossy().as_ref() == old)
                })
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
                Err(e) => return Err(StoreError::Invalid(format!("corrupt persisted quality: {e}"))),
            }
        }
    }

    let mut scoped = SubscribeFacts::default();
    for ((season, episode), fact) in facts.entries() {
        scoped.replace(season, episode, fact.clone());
        if let Some(path) = fact.path.as_deref() {
            if let Some(quality) = facts.quality(path) {
                scoped.set_quality(path.to_string(), quality.clone());
            }
        }
    }
    Ok(scoped)
}
