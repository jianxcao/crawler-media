//! Short Subscribe decisions; a round owns a marker, never a lock across IO.
use domain::{Subscribe, SubscribeId};

use crate::management::ApiState;

pub(super) struct ActiveTransfer<'a> {
    state: &'a ApiState,
    subscribe: SubscribeId,
}

impl<'a> ActiveTransfer<'a> {
    pub(super) fn begin(state: &'a ApiState, subscribe: SubscribeId) -> Option<Self> {
        let guard = state.subscribe_guard(subscribe);
        let _guard = guard.lock();
        if state.subscribe_is_deleting(subscribe) {
            return None;
        }
        if !state.active_transfers.lock().insert(subscribe) {
            return None;
        }
        Some(Self { state, subscribe })
    }
}

impl Drop for ActiveTransfer<'_> {
    fn drop(&mut self) {
        self.state.active_transfers.lock().remove(&self.subscribe);
        self.state.subscribe_round_finished.notify_waiters();
    }
}

/// Caller holds subscribe_guard before its Store decision; neither spans IO.
pub(super) fn ensure_current(
    state: &ApiState,
    store: &store::Store,
    expected: &Subscribe,
) -> Result<(), String> {
    if state.subscribe_is_deleting(expected.id)
        || store
            .get_subscribe(expected.id)
            .map_err(|error| error.to_string())?
            .as_ref()
            != Some(expected)
    {
        return Err("Transfer Subscribe changed or is being deleted".into());
    }
    Ok(())
}
