use super::{ApiState, StoreError};
use domain::SubscribeId;

/// Keeps a subscription marked as deleting across async cleanup awaits. Drop
/// clears the reservation after the caller has either deleted the row or
/// abandoned cleanup, so a failed/cancelled request does not strand it.
pub(crate) struct SubscribeDeletionReservation {
    state: ApiState,
    id: SubscribeId,
}

impl SubscribeDeletionReservation {
    pub(crate) fn new(state: ApiState, id: SubscribeId) -> Self {
        Self { state, id }
    }
}

impl Drop for SubscribeDeletionReservation {
    fn drop(&mut self) {
        let guard = self.state.subscribe_guard(self.id);
        let _guard = guard.lock();
        self.state.clear_subscribe_deleting(self.id);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiStateError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub(super) fn open_jobs(
    jobs_path: &std::path::Path,
    seed_defs: bool,
) -> Result<jobs::Queue, ApiStateError> {
    let jobs =
        jobs::Queue::open(jobs_path).map_err(|error| std::io::Error::other(error.to_string()))?;
    if seed_defs {
        seed_jobs(&jobs)?;
    }
    Ok(jobs)
}

pub(super) fn seed_jobs(jobs: &jobs::Queue) -> Result<(), ApiStateError> {
    crate::jobs_api::seed_instance_defs(jobs)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    // 维护任务默认 def：幂等注册，用户改过的开关/频率/payload 原样保留。
    crate::maintenance::seed_maintenance_defs(jobs)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(())
}
