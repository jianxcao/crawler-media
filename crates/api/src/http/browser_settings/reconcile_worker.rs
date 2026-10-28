//! One dirty, coalesced driver per ApiState; its lease follows the actual worker.

use crate::ApiState;
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
struct State {
    running: bool,
    dirty: bool,
}

#[derive(Default)]
pub(crate) struct ReconcileCoordinator {
    state: Mutex<State>,
}

struct Runner {
    owner: Arc<ReconcileCoordinator>,
    active: bool,
}

impl ReconcileCoordinator {
    fn request(self: &Arc<Self>) -> Option<Runner> {
        let mut state = self.state.lock();
        state.dirty = true;
        if state.running {
            return None;
        }
        state.running = true;
        Some(Runner {
            owner: self.clone(),
            active: true,
        })
    }
}

impl Runner {
    fn run(mut self, mut reconcile: impl FnMut()) {
        loop {
            self.owner.state.lock().dirty = false;
            reconcile();
            let mut state = self.owner.state.lock();
            if !state.dirty {
                state.running = false;
                self.active = false;
                return;
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if self.active {
            self.owner.state.lock().running = false;
        }
    }
}

pub(super) fn schedule(state: &ApiState) {
    let Some(runner) = state.browser_reconcile.request() else {
        return;
    };
    let owned = state.clone();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            // Only one driver waits per instance. Saved configuration is never
            // dropped because the request budget was temporarily full.
            let reservation = loop {
                match owned.http_work.try_reserve() {
                    Ok(reservation) => break reservation,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
                }
            };
            let result = reservation
                .run_to_completion(move || {
                    runner.run(|| super::reconcile_obscura(&owned));
                })
                .await;
            if let Err(error) = result {
                tracing::error!(%error, "Browser reconcile worker failed");
            }
        });
    } else {
        // Startup is already synchronous; it must not detach an untracked thread.
        runner.run(|| super::reconcile_obscura(&owned));
    }
}

#[cfg(test)]
mod tests;
