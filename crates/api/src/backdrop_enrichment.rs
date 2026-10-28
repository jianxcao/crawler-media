//! Best-effort background backdrops with bounded admission and one worker per target.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::blocking_budget::BlockingBudget;
use crate::management::ApiState;

pub(crate) struct BackdropEnrichment {
    budget: BlockingBudget,
    targets: Mutex<HashSet<PathBuf>>,
}

struct TargetLease {
    owner: Arc<BackdropEnrichment>,
    target: PathBuf,
}

impl Drop for TargetLease {
    fn drop(&mut self) {
        self.owner.targets.lock().remove(&self.target);
    }
}

impl BackdropEnrichment {
    pub(crate) fn new(concurrency: usize) -> Self {
        Self {
            budget: BlockingBudget::new(concurrency),
            targets: Mutex::new(HashSet::new()),
        }
    }

    /// Reserve before spawning, so busy requests leave no queued task or target.
    /// The worker owns both reservations through its real completion or unwind.
    pub(crate) fn schedule(
        self: &Arc<Self>,
        state: ApiState,
        media: domain::Media,
        video: PathBuf,
    ) {
        let Some(target) = crate::poster_fetch::backdrop_target(&video) else {
            return;
        };
        let reservation = {
            let mut targets = self.targets.lock();
            if targets.contains(&target) {
                return;
            }
            let Ok(reservation) = self.budget.try_reserve() else {
                tracing::debug!(path = %target.display(), "background backdrop capacity exhausted");
                return;
            };
            targets.insert(target.clone());
            reservation
        };
        let lease = TargetLease {
            owner: self.clone(),
            target: target.clone(),
        };
        let media_id = media.id;
        tokio::spawn(async move {
            let result = reservation
                .run_to_completion(move || {
                    let _lease = lease;
                    // The scheduler lock is gone before Registry → Store access.
                    crate::poster_fetch::attach_backdrop_outcome(&state, &media, &video)
                })
                .await;
            match result {
                Ok(outcome) => tracing::debug!(%media_id, path = %target.display(), ?outcome,
                    "background backdrop work finished"),
                Err(error) => tracing::error!(%error, %media_id, path = %target.display(),
                    "background backdrop worker failed"),
            }
        });
    }
}
