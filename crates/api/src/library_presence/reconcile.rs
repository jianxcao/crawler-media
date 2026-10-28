//! Observe one bounded ownership window outside Store, then publish against its scope.
use super::{PresenceError, PresencePublishError, publish_presence};
use crate::management::ApiState;
use domain::MediaId;
use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope,
};

pub(super) fn reconcile_presence(
    state: &ApiState,
    library_id: &str,
    target_media_id: Option<MediaId>,
    mode: LedgerPresenceMode,
) -> Result<LedgerPresenceResult, PresenceError> {
    reconcile(state, library_id, target_media_id, mode, None)
}

/// Same reconciliation, under a lease the caller (a scan) already holds.
pub(super) fn reconcile_presence_leased(
    state: &ApiState,
    library_id: &str,
    mode: LedgerPresenceMode,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<LedgerPresenceResult, PresenceError> {
    reconcile(state, library_id, None, mode, Some(lease))
}

fn reconcile(
    state: &ApiState,
    library_id: &str,
    target_media_id: Option<MediaId>,
    mode: LedgerPresenceMode,
    held: Option<&crate::library_lifecycle::LibraryWorkLease>,
) -> Result<LedgerPresenceResult, PresenceError> {
    let (target_lib, snapshot) = {
        let guard = state.store.lock();
        let snapshot = guard.library_ownership()?;
        let lib = snapshot
            .library(library_id)
            .cloned()
            .ok_or(PresenceError::Missing)?;
        if !snapshot.accepts_work(library_id) {
            return Err(PresenceError::Deleting);
        }
        (lib, snapshot)
    };
    let scope = LibraryPathScope::resolve(snapshot)?;
    if !scope.accepts_work(library_id) {
        return Err(PresenceError::Deleting);
    }
    for root in &target_lib.root_paths {
        if let Err(error) = state.library_files.readable_root(root) {
            tracing::warn!(root = %root.display(), %error, "Library root unavailable");
            return Err(PresenceError::RootUnavailable);
        }
    }
    let owned;
    let lease = match held {
        Some(lease) => lease,
        None => {
            owned = state
                .acquire_library_work(library_id)
                .map_err(|_| PresenceError::Deleting)?;
            &owned
        }
    };
    let id = library_id.parse().map_err(store::StoreError::from)?;
    let mut cursor = None;
    let mut total = LedgerPresenceResult::default();
    loop {
        let mut observations = publish_presence(lease, || {
            let store = state.store.lock();
            ensure_scope(&store, &scope, library_id)?;
            store
                .ledger_presence_page(id, target_media_id, cursor, 100)
                .map_err(Into::into)
        })?;
        let Some(last) = observations.last() else {
            break;
        };
        cursor = Some(last.file.file.file_id);
        observe_window(state, &mut observations, mode);
        let now = crate::job_loop::unix_now();
        let result = publish_presence(lease, || {
            let store = state.store.lock();
            ensure_scope(&store, &scope, library_id)?;
            store
                .apply_ledger_presence(&observations, mode, now)
                .map_err(Into::into)
        })?;
        // Durable cancellation/rearming committed with these exact file generations.
        // Never cancel from the observed input: it may have been skipped as stale.
        state.probe.presence_committed(&result.applied);
        prune_deleted_sidecars(state, &observations, &result);
        total.missing += result.missing;
        total.restored += result.restored;
        total.deleted += result.deleted;
        total.skipped += result.skipped;
    }
    tracing::info!(
        library_id,
        ?mode,
        missing = total.missing,
        restored = total.restored,
        deleted = total.deleted,
        skipped = total.skipped,
        "Library presence reconciliation completed"
    );
    Ok(total)
}

fn observe_window(
    state: &ApiState,
    observations: &mut [LedgerPresenceObservation],
    mode: LedgerPresenceMode,
) {
    for obs in observations {
        obs.presence = observe_file(state, obs);
        if mode == LedgerPresenceMode::Prune
            && obs.presence == FilePresence::Missing
            && obs.missing_at.is_some()
        {
            obs.presence = observe_file(state, obs);
        }
    }
}

fn observe_file(state: &ApiState, obs: &LedgerPresenceObservation) -> FilePresence {
    match state.library_files.presence(&obs.file.file.path) {
        Ok(true) => FilePresence::Present,
        Ok(false) => FilePresence::Missing,
        Err(error) => {
            tracing::warn!(path=%obs.file.file.path.display(), %error, "Observing file presence failed");
            FilePresence::Unknown
        }
    }
}

fn ensure_scope(
    store: &store::Store,
    scope: &LibraryPathScope,
    library_id: &str,
) -> Result<(), PresencePublishError> {
    let current = store.library_ownership()?;
    if !current.accepts_work(library_id) {
        return Err(PresencePublishError::LibraryDeleting);
    }
    if !scope.matches(&current) {
        return Err(PresencePublishError::ScopeChanged);
    }
    Ok(())
}

/// Rows the publish actually deleted also lose their on-disk sidecars and
/// emptied directories (outside the Store lock).
fn prune_deleted_sidecars(
    state: &ApiState,
    observations: &[LedgerPresenceObservation],
    result: &LedgerPresenceResult,
) {
    for applied in &result.applied {
        if applied.outcome != store::LedgerPresenceOutcome::Deleted {
            continue;
        }
        if let Some(obs) = observations
            .iter()
            .find(|obs| obs.file.file.file_id == applied.file_id)
        {
            crate::http::file_delete::prune_disk_leftovers(&state.store, &obs.file.file.path);
        }
    }
}
