use std::path::Path;

use domain::{MediaKind, Subscribe, UnitKey};
use store::{LibraryPathScope, StoreError, StrictOwner, SubscribeLibrarySnapshot};
use subscribe::{QualityFact, SubscribeFacts};

use crate::management::ApiState;

/// Evaluate SubscribeFacts out-of-lock using an owned DB snapshot.
///
/// Pure path-scope resolution and file verification run outside the Store lock.
pub fn evaluate_subscribe_facts_out_of_lock(
    snapshot: &SubscribeLibrarySnapshot,
) -> Result<SubscribeFacts, StoreError> {
    evaluate_subscribe_facts_out_of_lock_with(
        snapshot,
        &crate::browse_file_read::LocalBrowseFileRead,
    )
}

/// Evaluate SubscribeFacts out-of-lock using an owned DB snapshot and a file existence seam.
pub fn evaluate_subscribe_facts_out_of_lock_with(
    snapshot: &SubscribeLibrarySnapshot,
    files: &dyn crate::browse_file_read::BrowseFileRead,
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
        let Some(path_str) = fact.path.as_deref() else {
            continue;
        };
        let p = Path::new(path_str);
        let resolved = match LibraryPathScope::resolve_path(p) {
            Ok(r) => r,
            Err(e) => return Err(e),
        };

        match scope.owner(&resolved, snapshot.kind) {
            StrictOwner::Owned(owner) if owner.id == target_library.id => {
                facts.replace(season, episode, fact.clone());
                if let Some(quality) = snapshot.saved.quality(path_str) {
                    facts.set_quality(path_str.to_string(), quality.clone());
                }
            }
            _ => {
                tracing::info!(
                    subscribe_id = %snapshot.subscribe.id,
                    library_id = %target_library.id,
                    season,
                    episode,
                    path = path_str,
                    "已保存事实不属于目标 Library，本次不把它当作已拥有内容"
                );
            }
        }
    }

    merge_snapshot_rows(snapshot, files, &scope, &target_library.id, &mut facts)?;

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

/// Merge the snapshot's covered files into `facts`: every existing file owned by
/// the target Library contributes the facts of each `(file, unit)` it covers.
fn merge_snapshot_rows(
    snapshot: &SubscribeLibrarySnapshot,
    files: &dyn crate::browse_file_read::BrowseFileRead,
    scope: &LibraryPathScope,
    target_library_id: &str,
    facts: &mut SubscribeFacts,
) -> Result<(), StoreError> {
    for row in &snapshot.rows {
        let path = row.file.path.to_string_lossy().into_owned();
        let p = Path::new(&path);
        if !files.is_file(p) {
            continue;
        }

        let resolved = match LibraryPathScope::resolve_path(p) {
            Ok(r) => r,
            Err(e) => return Err(e),
        };

        if !matches!(scope.owner(&resolved, snapshot.kind), StrictOwner::Owned(owner) if owner.id == target_library_id)
        {
            continue;
        }

        let fact = QualityFact {
            score: row.file.filter_score.unwrap_or(0),
            path: Some(path.clone()),
        };
        // 一个物理文件可覆盖多个 unit；legacy 拿到的是逐 unit 的 join 行，这里逐 unit 保值。
        // 读 shape 下每个文件至少一个 unit（LEFT JOIN 缺失行读作 `UnitKey::WHOLE`），空 units 兜底同样按整片算。
        let whole = [UnitKey::WHOLE];
        let units: &[UnitKey] = if row.units.is_empty() {
            &whole
        } else {
            &row.units
        };
        for unit in units {
            let (season, episode) = match unit.episode_coordinates() {
                Some((season, episode)) => (Some(season), Some(episode)),
                None => (None, None),
            };
            // Check if older path for this key is missing from snapshot rows
            let stale = facts
                .get(season, episode)
                .and_then(|f| f.path.as_deref())
                .map(|old| {
                    !snapshot
                        .rows
                        .iter()
                        .any(|r| r.file.path.to_string_lossy().as_ref() == old)
                })
                .unwrap_or(false);

            if stale {
                facts.replace(season, episode, fact.clone());
            } else {
                facts.upsert(season, episode, fact.clone());
            }
        }

        if let Some(quality_res) = snapshot.qualities.get(&row.file.file_id) {
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
    Ok(())
}

/// An observed Subscribe facts result alongside the scoped read fingerprint
/// captured when the observation was taken.
#[derive(Clone, Debug)]
pub struct SubscribeLibraryObservation {
    pub facts: SubscribeFacts,
    pub fingerprint: store::ReadFingerprint,
    pub library_id: String,
    pub library_root: std::path::PathBuf,
    pub ownership: store::LibraryOwnership,
}

/// Read subscribe library facts and observation context out of the Store lock.
pub fn observe_subscribe_library_facts(
    state: &ApiState,
    subscribe: &Subscribe,
    kind: MediaKind,
) -> Result<SubscribeLibraryObservation, String> {
    let snapshot = {
        let store = state.store.lock();
        store
            .subscribe_library_snapshot(subscribe, kind)
            .map_err(|e| e.to_string())?
    };

    let target_library_id = match snapshot.subscribe.library_id {
        Some(id) => id.to_string(),
        None => snapshot
            .ownership
            .default_for(snapshot.kind)
            .map(|lib| lib.id.clone())
            .ok_or_else(|| "目标 Library 不存在".to_string())?,
    };

    let target_library = snapshot
        .ownership
        .library(&target_library_id)
        .ok_or_else(|| "目标 Library 不存在".to_string())?;

    let library_root = target_library
        .root_paths
        .first()
        .cloned()
        .unwrap_or_default();

    let facts = evaluate_subscribe_facts_out_of_lock_with(&snapshot, state.browse_files.as_ref())
        .map_err(|e| e.to_string())?;

    Ok(SubscribeLibraryObservation {
        facts,
        fingerprint: snapshot.fingerprint,
        library_id: target_library_id,
        library_root,
        ownership: snapshot.ownership,
    })
}

/// Read subscribe library facts out of the Store lock.
///
/// Synchronous function; callers are already running in a blocking worker or background task.
/// Takes one owned DB snapshot under the lock and resolves path-scope/files outside the lock.
pub fn read_subscribe_library_facts(
    state: &ApiState,
    subscribe: &Subscribe,
    kind: MediaKind,
) -> Result<SubscribeFacts, String> {
    observe_subscribe_library_facts(state, subscribe, kind).map(|obs| obs.facts)
}
