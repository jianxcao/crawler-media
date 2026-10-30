use domain::{MediaKind, Subscribe};
use store::{StoreError, SubscribeLibrarySnapshot};
use subscribe::SubscribeFacts;

use crate::management::ApiState;

/// Evaluate SubscribeFacts out-of-lock using an owned DB snapshot.
///
/// The ownership rule itself lives in `store::resolve_subscribe_facts`; this
/// module only supplies the presence seam so filesystem IO stays outside the
/// Store lock.
pub fn evaluate_subscribe_facts_out_of_lock(
    snapshot: &SubscribeLibrarySnapshot,
) -> Result<SubscribeFacts, StoreError> {
    evaluate_subscribe_facts_out_of_lock_with(snapshot, &crate::file_stat_read::LocalFileStatRead)
}

/// Evaluate SubscribeFacts out-of-lock using an owned DB snapshot and a file existence seam.
pub fn evaluate_subscribe_facts_out_of_lock_with(
    snapshot: &SubscribeLibrarySnapshot,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Result<SubscribeFacts, StoreError> {
    store::resolve_subscribe_facts(snapshot)?.evaluate(&|path| files.is_file(path))
}

/// An observed Subscribe facts result alongside the scoped read fingerprint
/// captured when the observation was taken.
#[derive(Clone, Debug)]
pub struct SubscribeLibraryObservation {
    pub facts: SubscribeFacts,
    pub fingerprint: store::ReadFingerprint,
    pub snapshot: SubscribeLibrarySnapshot,
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
        let store = state.store.as_ref();
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

    let facts = evaluate_subscribe_facts_out_of_lock_with(&snapshot, state.file_stats.as_ref())
        .map_err(|e| e.to_string())?;

    {
        let store = state.store.as_ref();
        if !store
            .subscribe_snapshot_is_current(&snapshot, &Default::default())
            .map_err(|e| e.to_string())?
        {
            return Err("Library 配置或台账在观察期间已变更，放弃本次读取".into());
        }
    }

    Ok(SubscribeLibraryObservation {
        facts,
        fingerprint: snapshot.fingerprint.clone(),
        library_id: target_library_id,
        library_root,
        ownership: snapshot.ownership.clone(),
        snapshot,
    })
}

/// Read subscribe library facts out of the Store lock.
///
/// Synchronous function; callers are already running in a blocking worker or background task.
/// Takes one owned DB snapshot under the lock and resolves file existence outside it.
pub fn read_subscribe_library_facts(
    state: &ApiState,
    subscribe: &Subscribe,
    kind: MediaKind,
) -> Result<SubscribeFacts, String> {
    observe_subscribe_library_facts(state, subscribe, kind).map(|obs| obs.facts)
}
