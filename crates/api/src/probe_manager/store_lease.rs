use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::Mutex;

use super::ProbeManager;

fn active() -> &'static Mutex<HashSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

pub(super) fn recover_if_idle(manager: &ProbeManager) -> bool {
    let active = active().lock();
    if active.contains(&manager.store_key) {
        return false;
    }
    manager.recover_pending_jobs();
    true
}

pub(super) fn acquire(key: PathBuf) -> Option<Lease> {
    if active().lock().insert(key.clone()) {
        Some(Lease(key))
    } else {
        None
    }
}

pub(super) struct Lease(PathBuf);

impl Drop for Lease {
    fn drop(&mut self) {
        active().lock().remove(&self.0);
    }
}
