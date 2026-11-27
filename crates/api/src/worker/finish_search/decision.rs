//! Scheduled search admission owns a marker, never a lock across Downloader IO.
use domain::{Subscribe, SubscribeId};

use crate::management::ApiState;

pub(super) struct ActiveSearch<'a> {
    state: &'a ApiState,
    id: SubscribeId,
}

impl<'a> ActiveSearch<'a> {
    pub(super) fn begin(state: &'a ApiState, id: SubscribeId) -> Option<Self> {
        if !state.active_transfers.lock().insert(id) {
            return None;
        }
        Some(Self { state, id })
    }
}

impl Drop for ActiveSearch<'_> {
    fn drop(&mut self) {
        self.state.active_transfers.lock().remove(&self.id);
        self.state.subscribe_round_finished.notify_waiters();
    }
}

pub(super) fn ensure_current(state: &ApiState, expected: &Subscribe) -> Result<(), String> {
    if expected.tracking_state == "paused"
        || state.subscribe_is_deleting(expected.id)
        || state
            .store
            .get_subscribe(expected.id)
            .map_err(|error| error.to_string())?
            .as_ref()
            != Some(expected)
    {
        return Err("Search Subscribe changed, paused or is being deleted".into());
    }
    Ok(())
}
