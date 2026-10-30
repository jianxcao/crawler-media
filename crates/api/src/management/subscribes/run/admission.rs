//! Manual admission keeps ownership across IO without retaining a Subscribe lock.
use domain::{Subscribe, SubscribeId, Torrent};
use downloader::{Downloader, DownloaderError};

use crate::delivery::RoutedDownloader;
use crate::management::{ApiError, ApiState};

pub(super) struct ActiveRun<'a> {
    state: &'a ApiState,
    id: SubscribeId,
}

impl<'a> ActiveRun<'a> {
    pub(super) fn begin(state: &'a ApiState, id: SubscribeId) -> Result<Self, ApiError> {
        if !state.active_transfers.lock().insert(id) {
            return Err(super::lifecycle_conflict(
                "subscription.running",
                "订阅正在处理，无法重复运行",
            ));
        }
        Ok(Self { state, id })
    }
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        self.state.active_transfers.lock().remove(&self.id);
        self.state.subscribe_round_finished.notify_waiters();
    }
}

/// Caller holds the Subscribe guard only while validating/publishing SQL facts.
pub(super) fn ensure_current(state: &ApiState, expected: &Subscribe) -> Result<(), ApiError> {
    super::reject_if_inactive(state, expected.id, expected)?;
    if state.store.get_subscribe(expected.id)?.as_ref() != Some(expected) {
        return Err(super::lifecycle_conflict(
            "subscription.changed",
            "订阅配置已变更，跳过本轮结果",
        ));
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
            ensure_current(self.state, self.expected).map_err(|_| {
                DownloaderError::Message("Subscribe changed before download submission".into())
            })?;
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

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<std::path::PathBuf>, bool), DownloaderError> {
        self.routed.collectable_files(torrent)
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        self.routed.is_claimed(torrent)
    }
}

/// An accepted remote task remains tracked if paused during IO. A deleted
/// Subscribe must never be recreated by a late response from its Downloader.
pub(super) fn record_accepted(
    state: &ApiState,
    id: SubscribeId,
    outcome: &subscribe::RunOutcome,
    routed: &RoutedDownloader<'_>,
) -> Result<(), ApiError> {
    let guard = state.subscribe_guard(id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(id) || state.store.get_subscribe(id)?.is_none() {
        return Err(super::lifecycle_conflict(
            "subscription.deleting",
            "订阅正在删除或已不存在，跳过本轮结果",
        ));
    }
    let now = crate::job_loop::unix_now();
    let pending: Vec<_> = outcome
        .torrents_added
        .iter()
        .map(|(score, torrent)| {
            (
                *score,
                crate::store::PendingDownload {
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id: routed.used_id(torrent),
                    submitted_at: Some(now),
                },
            )
        })
        .collect();
    state.store.record_pending_submissions(id, &pending)?;
    for (_, item) in &pending {
        if let Some(hash) = routed.submitted_hash(&item.torrent) {
            state
                .store
                .set_pending_info_hash(id, &item.torrent.enclosure, &hash)?;
        }
    }
    Ok(())
}
