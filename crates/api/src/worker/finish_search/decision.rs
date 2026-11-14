//! Scheduled search admission owns a marker, never a lock across Downloader IO.
use domain::{Subscribe, SubscribeId, Torrent};
use downloader::{Downloader, DownloaderError};

use crate::delivery::RoutedDownloader;
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

pub(super) struct AdmittedDownloader<'a, 'b> {
    pub(super) state: &'a ApiState,
    pub(super) expected: &'a Subscribe,
    pub(super) routed: &'a RoutedDownloader<'b>,
    pub(super) control: &'a library::WorkControl,
}

impl Downloader for AdmittedDownloader<'_, '_> {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        {
            let guard = self.state.subscribe_guard(self.expected.id);
            let _guard = guard.lock();
            ensure_current(self.state, self.expected).map_err(DownloaderError::Message)?;
        }
        self.control
            .check()
            .map_err(|error| DownloaderError::Message(error.to_string()))?;
        self.routed.add(torrent)
    }

    fn completed_files(
        &self,
        torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        self.routed.completed_files(torrent)
    }
}
