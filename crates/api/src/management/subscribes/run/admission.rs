//! Manual admission keeps ownership across IO without retaining a Subscribe lock.
use domain::{Subscribe, SubscribeId};

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
pub(crate) fn ensure_current(state: &ApiState, expected: &Subscribe) -> Result<(), ApiError> {
    super::reject_if_inactive(state, expected.id, expected)?;
    if state.store.get_subscribe(expected.id)?.as_ref() != Some(expected) {
        return Err(super::lifecycle_conflict(
            "subscription.changed",
            "订阅配置已变更，跳过本轮结果",
        ));
    }
    Ok(())
}

/// An accepted remote task remains tracked if paused during IO. Definitive
/// source rejections (`SourceRejected` / `ExistingTaskBroken`) are also
/// persisted immediately so an unrelated transient error on another candidate
/// never drops them. A deleted Subscribe must never be recreated by a late
/// response from its Downloader.
pub(super) fn record_accepted(
    state: &ApiState,
    (expected, library_id, policy_binding): (
        &Subscribe,
        &str,
        &Option<store::SubscribePolicyBinding>,
    ),
    outcome: &subscribe::RunOutcome,
    broken_scored: &[filter::ScoredTorrent],
    unknown: &[filter::ScoredTorrent],
    select_deferred: &[(filter::ScoredTorrent, i64, String)],
    routed: &RoutedDownloader<'_>,
) -> Result<(), ApiError> {
    let id = expected.id;
    let guard = state.subscribe_guard(id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(id) || state.store.get_subscribe(id)?.is_none() {
        return Err(super::lifecycle_conflict(
            "subscription.deleting",
            "订阅正在删除或已不存在，跳过本轮结果",
        ));
    }
    let now = crate::job_loop::unix_now();
    let pending = build_pending_submissions(outcome, unknown, routed, now);
    state.store.record_pending_submissions(id, &pending)?;
    for (_, item) in &pending {
        if let Some(hash) = outcome
            .submitted_meta
            .get(&item.torrent.enclosure)
            .and_then(|(_, h)| h.as_deref())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| routed.submitted_hash(&item.torrent))
            .filter(|s| !s.is_empty())
        {
            state
                .store
                .set_pending_info_hash(id, &item.torrent.enclosure, &hash)?;
        }
    }
    persist_broken_submissions(&state.store, id, outcome, broken_scored, routed, now)?;
    if !select_deferred.is_empty()
        && ensure_current(state, expected).is_ok()
        && !state.library_work().is_cancelled(library_id)
        && state.store.library_accepts_work(library_id)?
    {
        crate::worker::finish_search::record_select_defers(
            state,
            id,
            policy_binding,
            select_deferred,
            now,
        );
    }
    Ok(())
}

fn build_pending_submissions(
    outcome: &subscribe::RunOutcome,
    unknown: &[filter::ScoredTorrent],
    routed: &RoutedDownloader<'_>,
    now: i64,
) -> Vec<(i32, crate::store::PendingDownload)> {
    let mut pending: Vec<_> = outcome
        .torrents_added
        .iter()
        .map(|(score, torrent)| {
            let downloader_id = outcome
                .submitted_meta
                .get(&torrent.enclosure)
                .and_then(|(id, _)| *id)
                .or_else(|| routed.used_id(torrent));
            (
                *score,
                crate::store::PendingDownload {
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id,
                    submitted_at: Some(now),
                },
            )
        })
        .collect();
    for scored in unknown {
        let downloader_id = outcome
            .submitted_meta
            .get(&scored.torrent.enclosure)
            .and_then(|(id, _)| *id)
            .or_else(|| routed.used_id(&scored.torrent));
        pending.push((
            scored.score,
            crate::store::PendingDownload {
                torrent: scored.torrent.clone(),
                release_override: None,
                downloader_id,
                submitted_at: Some(0),
            },
        ));
    }
    pending
}

fn persist_broken_submissions(
    store: &crate::Store,
    id: SubscribeId,
    outcome: &subscribe::RunOutcome,
    broken_scored: &[filter::ScoredTorrent],
    routed: &RoutedDownloader<'_>,
    now: i64,
) -> Result<(), ApiError> {
    let mut broken: Vec<(i32, crate::store::PendingDownload)> = broken_scored
        .iter()
        .map(|scored| {
            (
                scored.score,
                crate::store::PendingDownload {
                    torrent: scored.torrent.clone(),
                    release_override: None,
                    downloader_id: routed.used_id(&scored.torrent),
                    submitted_at: Some(now),
                },
            )
        })
        .collect();
    for torrent in routed.broken_torrents() {
        if !broken
            .iter()
            .any(|(_, p)| p.torrent.enclosure == torrent.enclosure)
        {
            let downloader_id = routed.used_id(&torrent);
            broken.push((
                0,
                crate::store::PendingDownload {
                    torrent,
                    release_override: None,
                    downloader_id,
                    submitted_at: Some(now),
                },
            ));
        }
    }
    if !broken.is_empty() || !outcome.rejected_enclosures.is_empty() {
        let broken_enclosures: Vec<String> = broken
            .iter()
            .map(|(_, p)| p.torrent.enclosure.clone())
            .collect();
        store.record_pending_submissions(id, &broken)?;
        store.mark_pending_rejected(id, &outcome.rejected_enclosures)?;
        store.mark_pending_rejected(id, &broken_enclosures)?;
    }
    Ok(())
}
