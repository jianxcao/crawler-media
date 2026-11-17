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
        let guard = state.store.as_ref();
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
            let store = state.store.as_ref();
            ensure_scope(&store, &scope, library_id)?;
            store
                .ledger_presence_page(id, target_media_id, cursor, 100)
                .map_err(Into::into)
        })?;
        let Some(last) = observations.last() else {
            break;
        };
        cursor = Some(last.file.file.file_id);
        let _pending_claims = observe_window(state, &mut observations, mode);
        // Register before removing file facts: Library removal must still find
        // these operations after their live rows disappear. Wait outside the
        // publication gate, then revalidate lifecycle and ownership below.
        let _claims = claim_missing_paths(&observations, mode);
        let now = crate::job_loop::unix_now();
        let result = publish_presence(lease, || {
            let store = state.store.as_ref();
            ensure_scope(&store, &scope, library_id)?;
            store
                .apply_ledger_presence(&observations, mode, now)
                .map_err(Into::into)
        })?;
        // Durable cancellation/rearming committed with these exact file generations.
        // Never cancel from the observed input: it may have been skipped as stale.
        state.probe.presence_committed(&result.applied);
        prune_deleted_sidecars(state, lease, &scope, &observations, &result)?;
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

fn claim_missing_paths(
    observations: &[LedgerPresenceObservation],
    mode: LedgerPresenceMode,
) -> Vec<crate::http::file_delete::PathClaimGuard> {
    let mut keys: Vec<_> = observations
        .iter()
        .filter(|obs| {
            obs.file.file.publication != domain::FilePublication::Pending
                && obs.presence == FilePresence::Missing
                && (mode == LedgerPresenceMode::Purge
                    || (mode == LedgerPresenceMode::Prune && obs.missing_at.is_some()))
        })
        .map(|obs| obs.path_key.as_str())
        .collect();
    // Concurrent presence windows can overlap; acquire every path in one order.
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter()
        .map(|key| crate::http::file_delete::path_claim(key).lock())
        .collect()
}

fn observe_window(
    state: &ApiState,
    observations: &mut [LedgerPresenceObservation],
    mode: LedgerPresenceMode,
) -> Vec<crate::http::file_delete::PathClaimGuard> {
    let mut pending_claims = Vec::new();
    for obs in observations {
        if obs.file.file.publication == domain::FilePublication::Pending {
            // A live Transfer owns its reservation from prepare until completion.
            // Only an unclaimed pending destination is an interrupted placement.
            let claim = crate::http::file_delete::transfer_claim(&obs.path_key);
            let Some(guard) = claim.try_lock() else {
                obs.presence = FilePresence::Unknown;
                continue;
            };
            pending_claims.push(guard);
        }
        obs.presence = observe_file(state, obs);
        if obs.file.file.publication == domain::FilePublication::Pending
            && obs.presence == FilePresence::Present
        {
            match library::observe_source(&obs.file.file.path) {
                Ok(source) => obs.observed_source = Some(source),
                Err(error) => {
                    tracing::warn!(path=%obs.file.file.path.display(), %error, "Observing pending Transfer source failed");
                    obs.presence = FilePresence::Unknown;
                }
            }
        }
        if mode == LedgerPresenceMode::Prune
            && obs.presence == FilePresence::Missing
            && obs.missing_at.is_some()
        {
            obs.presence = observe_file(state, obs);
        }
    }
    pending_claims
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
/// emptied directories. Claims remain active through IO, but no mutex or
/// publication gate is held while the filesystem is accessed.
fn prune_deleted_sidecars(
    state: &ApiState,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    scope: &LibraryPathScope,
    observations: &[LedgerPresenceObservation],
    result: &LedgerPresenceResult,
) -> Result<(), PresenceError> {
    for applied in &result.applied {
        if applied.outcome != store::LedgerPresenceOutcome::Deleted {
            continue;
        }
        if let Some(obs) = observations
            .iter()
            .find(|obs| obs.file.file.file_id == applied.file_id)
        {
            publish_presence(lease, || {
                ensure_scope(&state.store, scope, lease.library_id())
            })?;
            crate::http::file_delete::prune_disk_leftovers(&state.store, &obs.file.file.path);
        }
    }
    Ok(())
}
