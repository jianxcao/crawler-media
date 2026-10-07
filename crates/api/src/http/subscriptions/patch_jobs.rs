use crate::management::ApiState;

pub(super) fn sync_jobs(
    state: &ApiState,
    subscribe_id: &str,
    paused: bool,
    interval: u32,
) -> Result<(), jobs::JobError> {
    let queue = state.jobs.lock();
    crate::http::subscribe_schedule::sync_search_job(&queue, subscribe_id, paused)?;
    crate::http::subscribe_schedule::set_search_interval(&queue, subscribe_id, interval)
}

pub(super) fn rollback_jobs(state: &ApiState, subscribe_id: &str, previous: &domain::Subscribe) {
    if let Err(error) = sync_jobs(
        state,
        subscribe_id,
        previous.tracking_state == "paused",
        previous.search_interval_secs,
    ) {
        tracing::error!(subscribe_id, error = %error, "队列同步失败后回滚 Job 状态失败");
    }
}
