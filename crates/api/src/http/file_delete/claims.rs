//! Same-path drains must not overlap: the first worker may complete its intent
//! before a second worker reaches unlink. Hold this claim through the final SQL.
//! Admission order is path claim -> Store; the registry is never held while
//! waiting. Reservations retain no mutex guard during database or filesystem IO.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::{Condvar, Mutex};

type ClaimRegistry = OnceLock<Mutex<HashMap<String, Weak<PathClaim>>>>;

static CLAIMS: ClaimRegistry = OnceLock::new();
// Transfer placement is cancellable Library work, not a physical deletion.
// Removal must not wait for these claims when closing admitted destructive IO.
static TRANSFER_CLAIMS: ClaimRegistry = OnceLock::new();

#[derive(Default)]
pub(crate) struct PathClaim {
    active: Mutex<bool>,
    changed: Condvar,
}

pub(crate) struct PathClaimGuard(Arc<PathClaim>);

impl PathClaim {
    pub(crate) fn try_lock(self: &Arc<Self>) -> Option<PathClaimGuard> {
        let mut active = self.active.lock();
        if *active {
            return None;
        }
        *active = true;
        Some(PathClaimGuard(self.clone()))
    }

    pub(crate) fn lock(self: &Arc<Self>) -> PathClaimGuard {
        let mut active = self.active.lock();
        while *active {
            self.changed.wait(&mut active);
        }
        *active = true;
        PathClaimGuard(self.clone())
    }
}

impl Drop for PathClaimGuard {
    fn drop(&mut self) {
        *self.0.active.lock() = false;
        self.0.changed.notify_all();
    }
}

pub(crate) fn path_claim(path_key: &str) -> Arc<PathClaim> {
    registered_claim(&CLAIMS, path_key)
}

pub(crate) fn transfer_claim(path_key: &str) -> Arc<PathClaim> {
    registered_claim(&TRANSFER_CLAIMS, path_key)
}

fn registered_claim(registry: &ClaimRegistry, path_key: &str) -> Arc<PathClaim> {
    let mut claims = registry.get_or_init(Mutex::default).lock();
    claims.retain(|_, claim| claim.strong_count() != 0);
    if let Some(claim) = claims.get(path_key).and_then(Weak::upgrade) {
        return claim;
    }
    let claim = Arc::new(PathClaim::default());
    claims.insert(path_key.to_string(), Arc::downgrade(&claim));
    claim
}

/// Snapshot active operations even if their file and queue rows are already gone.
/// The caller releases the registry before checking ownership or waiting.
pub(super) fn active_path_claims() -> Vec<(String, Arc<PathClaim>)> {
    CLAIMS
        .get_or_init(Mutex::default)
        .lock()
        .iter()
        .filter_map(|(key, claim)| claim.upgrade().map(|claim| (key.clone(), claim)))
        .collect()
}
