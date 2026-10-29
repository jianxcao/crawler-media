//! At most one automatic refresh leaves filesystem capacity for foreground reads.
use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::LibraryCoverService;
use crate::ApiState;

pub(super) struct RefreshQueue {
    pending: Mutex<HashSet<String>>,
    capacity: Arc<Semaphore>,
}

impl Default for RefreshQueue {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashSet::new()),
            capacity: Arc::new(Semaphore::new(1)),
        }
    }
}

impl LibraryCoverService {
    /// A native image GET returns its default immediately. Rendering and CAS
    /// publication run independently and retain the existing lifecycle guards.
    pub(crate) fn refresh_in_background(self: &Arc<Self>, state: &ApiState, id: &str) {
        if state.require_writable().is_err() || state.require_write_admitted().is_err() {
            return;
        }
        let mut pending = self.refresh.pending.lock();
        if pending.contains(id) {
            return;
        }
        if pending.len() >= 64 {
            tracing::warn!(library_id = %id, "Library cover refresh queue full");
            return;
        }
        pending.insert(id.to_owned());
        let service = self.clone();
        let state = state.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let _flight = RefreshFlight {
                service: service.clone(),
                id: id.clone(),
            };
            let Ok(_permit) = service.refresh.capacity.clone().acquire_owned().await else {
                return;
            };
            if let Err(error) = service.ensure_cover(&state, &id).await {
                tracing::warn!(%error, library_id = %id, "Background Library cover refresh failed");
            }
        });
    }
}

struct RefreshFlight {
    service: Arc<LibraryCoverService>,
    id: String,
}

impl Drop for RefreshFlight {
    fn drop(&mut self) {
        self.service.refresh.pending.lock().remove(&self.id);
    }
}
