use domain::Torrent;
use downloader::{Downloader, DownloaderError};

use crate::management::ApiState;

use super::deletion::TorrentRemovalTarget;

/// Identity of the physical Downloader task a pending row names.
///
/// `Some` when the client still holds the task (live hash) or when the
/// submission itself proves the hash even though the task is already gone
/// (magnet). `None` means the row cannot be proven, so cleanup must refuse
/// instead of guessing from titles, sizes, or enclosure strings.
pub(super) fn proven_task_identity(
    client: &dyn Downloader,
    torrent: &Torrent,
) -> Result<Option<String>, DownloaderError> {
    Ok(match client.owned_identity(torrent)? {
        Some(hash) => Some(hash),
        None => downloader::magnet_info_hash(&torrent.enclosure),
    })
}

/// Whether this task must be preserved because a surviving reference still needs it.
pub(super) fn is_task_protected(
    state: &ApiState,
    target: &TorrentRemovalTarget,
    client: &dyn Downloader,
    target_identity: Option<&str>,
    planned_endpoint: Option<&str>,
    deleting_enclosures: &std::collections::HashSet<(domain::SubscribeId, String)>,
) -> Result<bool, DownloaderError> {
    let identity = target_identity.ok_or_else(|| {
        downloader::unproven("target download identity could not be uniquely verified")
    })?;
    for (id, pending) in pending_references(state)? {
        // If this reference is itself scheduled for deletion in the current
        // batch, it cannot protect the physical task (both are being removed).
        if deleting_enclosures.contains(&(id, pending.torrent.enclosure.clone())) {
            continue;
        }
        if !shares_planned_route(
            state,
            pending.downloader_id,
            target.downloader_id,
            planned_endpoint,
        )? {
            continue;
        }
        if pending.torrent.enclosure == target.torrent.enclosure {
            tracing::warn!(torrent = %target.torrent.title, subscribe_id = %id,
                "preserving download still referenced by a surviving entry");
            return Ok(true);
        }
        match proven_task_identity(client, &pending.torrent)? {
            Some(shared) if shared == identity => {
                tracing::warn!(torrent = %target.torrent.title, subscribe_id = %id,
                    "preserving download with shared identity");
                return Ok(true);
            }
            Some(_) => {}
            None => {
                return Err(downloader::unproven(
                    "another entry references this Downloader route without a proven identity",
                ));
            }
        }
    }
    Ok(false)
}

/// Whether a surviving pending route still addresses the planned client.
///
/// The target endpoint is the snapshot taken at plan time. Re-resolving the
/// target after a default-downloader switch would make an explicit survivor on
/// the original client look distinct from the planned delete.
fn shares_planned_route(
    state: &ApiState,
    pending: Option<domain::DownloaderId>,
    target: Option<domain::DownloaderId>,
    planned_endpoint: Option<&str>,
) -> Result<bool, DownloaderError> {
    if pending == target {
        return Ok(true);
    }
    match (route_endpoint(state, pending)?, planned_endpoint) {
        (Some(pending_endpoint), Some(planned)) => Ok(pending_endpoint == planned),
        // A client that cannot report its endpoint cannot be proven distinct:
        // treat the routes as shared and let the identity comparison decide.
        _ => Ok(true),
    }
}

/// Endpoint of the client that this route actually selects at runtime.
///
/// Asking the connected client (rather than re-reading configuration) means a
/// startup-parsed override, including a secret read from a file, is honored
/// exactly like the real delivery and cleanup paths.
pub(super) fn route_endpoint(
    state: &ApiState,
    id: Option<domain::DownloaderId>,
) -> Result<Option<String>, DownloaderError> {
    let client = crate::delivery::frozen_client_for_id(state, id)?;
    let endpoint = client.endpoint();
    if endpoint.is_none() {
        tracing::warn!(?id, "downloader client cannot report its endpoint");
    }
    Ok(endpoint)
}

type PendingReference = (domain::SubscribeId, crate::store::PendingDownload);

fn pending_references(state: &ApiState) -> Result<Vec<PendingReference>, DownloaderError> {
    let store = state.store.lock();
    let result = (|| {
        let mut references = Vec::new();
        for subscribe in store.list_all_subscribes()? {
            let mut pending = store.load_pending(subscribe.id)?;
            pending.extend(store.load_pending_state(subscribe.id, "imported")?);
            references.extend(
                pending
                    .into_iter()
                    .map(|(_, pending)| (subscribe.id, pending)),
            );
        }
        Ok::<_, crate::store::StoreError>(references)
    })();
    result.map_err(|error| {
        tracing::error!(%error, "cannot prove exclusive download ownership");
        DownloaderError::Message(format!(
            "cannot prove exclusive download ownership: {error}"
        ))
    })
}
