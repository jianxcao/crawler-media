//! Resolve Torrent download URLs and keep each pending task on its Downloader.

use std::collections::HashMap;
use std::sync::Arc;

use domain::{DownloaderId, Subscribe, Torrent};
use downloader::{Downloader, DownloaderError, QbitDownloader, TransmissionDownloader};
use parking_lot::Mutex;

use crate::management::ApiState;
use crate::runtime_downloader::{ChosenDownloader, DownloaderEnv, row_to_chosen};

fn error(message: impl ToString) -> DownloaderError {
    DownloaderError::Message(message.to_string())
}

/// A persisted id is used only while it remains enabled for new deliveries.
/// Existing pending tasks can still use a Downloader that was later disabled.
fn enabled_id(
    store: &crate::Store,
    id: DownloaderId,
) -> Result<Option<DownloaderId>, DownloaderError> {
    match store.get_downloader(id).map_err(error)? {
        Some(row) if row.enabled => Ok(Some(id)),
        _ => {
            tracing::warn!(%id, "配置的下载器不可用，改用下一个符合条件的下载目标");
            Ok(None)
        }
    }
}

pub(crate) fn target_id(
    state: &ApiState,
    subscribe: Option<&Subscribe>,
    torrent: &Torrent,
) -> Result<Option<DownloaderId>, DownloaderError> {
    let store = state.store.clone();
    target_id_from_store(&store, subscribe, torrent)
}

pub(crate) fn target_id_from_store(
    store: &crate::Store,
    subscribe: Option<&Subscribe>,
    torrent: &Torrent,
) -> Result<Option<DownloaderId>, DownloaderError> {
    if let Some(id) = subscribe.and_then(|sub| sub.downloader_id)
        && let Some(id) = enabled_id(&store, id)?
    {
        return Ok(Some(id));
    }
    if let Some(id) = store
        .get_site(torrent.site_id)
        .map_err(error)?
        .and_then(|site| site.downloader_id)
        && let Some(id) = enabled_id(&store, id)?
    {
        return Ok(Some(id));
    }
    Ok(store.default_downloader().map_err(error)?.map(|row| row.id))
}

pub(crate) fn client_for_store_id(
    store: &crate::Store,
    default_dl: &Arc<dyn Downloader>,
    id: Option<DownloaderId>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    let Some(id) = id else {
        return Ok(default_dl.clone());
    };
    if id.as_uuid().is_nil() {
        return Ok(default_dl.clone());
    }
    let row = {
        let default_row = store.default_downloader().map_err(error)?;
        if default_row.as_ref().is_some_and(|row| row.id == id) {
            return Ok(default_dl.clone());
        }
        store
            .get_downloader(id)
            .map_err(error)?
            .ok_or_else(|| error(format!("Downloader {id} no longer exists")))?
    };
    match row_to_chosen(row, &DownloaderEnv::default()) {
        ChosenDownloader::Qbittorrent(config) => {
            QbitDownloader::connect(config).map(|client| Arc::new(client) as Arc<dyn Downloader>)
        }
        ChosenDownloader::Transmission(config) => TransmissionDownloader::connect(config)
            .map(|client| Arc::new(client) as Arc<dyn Downloader>),
        ChosenDownloader::Memory | ChosenDownloader::Unavailable => {
            Err(error(format!("Downloader {id} is incomplete")))
        }
    }
}

pub(crate) fn frozen_client_for_store_id(
    store: &crate::Store,
    default_dl: &Arc<dyn Downloader>,
    id: Option<DownloaderId>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    let client = client_for_store_id(store, default_dl, id)?;
    Ok(client.snapshot_client()?.unwrap_or(client))
}

pub(crate) fn client_for_id(
    state: &ApiState,
    id: Option<DownloaderId>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    client_for_store_id(&state.store, &state.downloader, id)
}

/// Concrete client for a route, frozen at this moment.
///
/// The default route may be a runtime proxy that re-selects configuration on
/// every call. Cleanup must snapshot the underlying connection so a later
/// switch cannot retarget a planned delete.
pub(crate) fn frozen_client_for_id(
    state: &ApiState,
    id: Option<DownloaderId>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    let client = client_for_id(state, id)?;
    Ok(client.snapshot_client()?.unwrap_or(client))
}

pub(crate) struct RoutedDownloader<'a> {
    state: &'a ApiState,
    subscribe: Option<&'a Subscribe>,
    pending: HashMap<String, Option<DownloaderId>>,
    used: Mutex<HashMap<String, Option<DownloaderId>>>,
    clients: Mutex<HashMap<Option<DownloaderId>, Arc<dyn Downloader>>>,
    /// D-M3: info hash reported by the client right after each submission.
    hashes: Mutex<HashMap<String, String>>,
    /// G-M2: torrents whose existing client task turned out broken.
    broken: Mutex<Vec<Torrent>>,
}

impl<'a> RoutedDownloader<'a> {
    pub(crate) fn new(state: &'a ApiState, subscribe: Option<&'a Subscribe>) -> Self {
        Self {
            state,
            subscribe,
            pending: HashMap::new(),
            used: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            broken: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn with_pending(
        state: &'a ApiState,
        subscribe: &'a Subscribe,
        pending: &[(i32, crate::store::PendingDownload)],
    ) -> Self {
        let targets = pending
            .iter()
            .map(|(_, item)| (item.torrent.enclosure.clone(), item.downloader_id))
            .collect();
        Self {
            state,
            subscribe: Some(subscribe),
            pending: targets,
            used: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            broken: Mutex::new(Vec::new()),
        }
    }

    /// D-M3: info hash of a torrent submitted through this router, if known.
    pub(crate) fn submitted_hash(&self, torrent: &Torrent) -> Option<String> {
        self.hashes.lock().get(&torrent.enclosure).cloned()
    }

    pub(crate) fn record_submitted_meta(
        &self,
        enclosure: &str,
        downloader_id: Option<DownloaderId>,
        info_hash: Option<String>,
    ) {
        self.used
            .lock()
            .insert(enclosure.to_string(), downloader_id);
        if let Some(hash) = info_hash {
            self.hashes.lock().insert(enclosure.to_string(), hash);
        }
    }

    /// G-M2: torrents rejected because their existing task is broken.
    pub(crate) fn broken_torrents(&self) -> Vec<Torrent> {
        self.broken.lock().clone()
    }

    pub(crate) fn used_id(&self, torrent: &Torrent) -> Option<DownloaderId> {
        self.used.lock().get(&torrent.enclosure).copied().flatten()
    }

    pub(crate) fn imported_destinations(
        &self,
    ) -> Result<Vec<subscribe::collection_destinations::DestinationMapping>, DownloaderError> {
        let Some(subscribe) = self.subscribe else {
            return Ok(Vec::new());
        };
        let store = self.state.store.clone();
        let media = store
            .get_media(subscribe.media_id)
            .map_err(error)?
            .ok_or_else(|| error("Media not found"))?;
        let library_id = subscribe.library_id.map(|id| id.to_string());
        // The transfer root still decides *where* a new file lands and guards a
        // Library that is being deleted; the sources are the target Library's
        // recorded files, never the files whose path happens to match. A root
        // rename cannot retarget a delivery this way.
        let _ =
            crate::directory::transfer_plan_for_library(&store, media.kind, library_id.as_deref())
                .map_err(error)?;
        let target_library = match subscribe.library_id {
            Some(id) => id,
            None => store
                .default_library(media.kind)
                .map_err(error)?
                .ok_or_else(|| error("target Library not found"))?
                .id
                .parse::<domain::LibraryId>()
                .map_err(error)?,
        };
        let mut sources = Vec::new();
        for coverage in store
            .file_coverage_in_library(
                target_library,
                Some(subscribe.media_id),
                crate::store::FileReadScope::AllRecorded,
            )
            .map_err(error)?
        {
            append_coverage_destinations(&store, &coverage, &mut sources)?;
        }
        Ok(sources)
    }

    fn client_for(&self, torrent: &Torrent) -> Result<Arc<dyn Downloader>, DownloaderError> {
        let id = self
            .pending
            .get(&torrent.enclosure)
            .copied()
            .or_else(|| self.used.lock().get(&torrent.enclosure).copied())
            .unwrap_or(None);
        if let Some(client) = self.clients.lock().get(&id) {
            return Ok(client.clone());
        }
        let client = client_for_id(self.state, id)?;
        self.clients.lock().insert(id, client.clone());
        Ok(client)
    }
}

fn append_coverage_destinations(
    store: &crate::Store,
    coverage: &domain::FileCoverage,
    sources: &mut Vec<subscribe::collection_destinations::DestinationMapping>,
) -> Result<(), DownloaderError> {
    let path = coverage.file.path.as_path();
    let Some(source) = coverage.file.source_path.as_deref() else {
        return Ok(());
    };
    let mut file_quality = release::parse(
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default(),
    );
    if coverage.file.resolution.is_some() {
        file_quality.resolution = coverage.file.resolution.clone();
    }
    if coverage.file.codec.is_some() {
        file_quality.codec = coverage.file.codec.clone();
    }
    if coverage.file.hdr.is_some() {
        file_quality.hdr = coverage.file.hdr.clone();
    }
    let transfer_pending = store
        .transfer_pending(coverage.file.path.to_string_lossy().as_ref())
        .map_err(error)?;
    for unit in &coverage.units {
        let mut slots = Vec::new();
        if let Some((from, to)) = file_quality.episode_span() {
            for ep in from..=to {
                slots.push((file_quality.season, Some(ep)));
            }
        }
        if slots.is_empty() {
            slots.push(match unit.episode_coordinates() {
                Some((season, episode)) => (Some(season), Some(episode)),
                None => (None, None),
            });
        }
        let mut mapping =
            subscribe::collection_destinations::DestinationMapping::with_slots_and_quality(
                source.to_string_lossy().into_owned(),
                path,
                slots,
                Some(file_quality.clone()),
            );
        mapping.transfer_pending = transfer_pending;
        sources.push(mapping);
    }
    Ok(())
}

impl Downloader for RoutedDownloader<'_> {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.client_for(torrent)?.remember_identity(torrent, hash)
    }

    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        self.client_for(torrent)?.select_files(torrent, keep)
    }

    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        client_for_id(self.state, None)?.submit_verified(payload, options)
    }

    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<downloader::ClientTaskQuery, DownloaderError> {
        client_for_id(self.state, None)?.query_task_status(info_hash, ownership_tag)
    }

    fn supports_labels(&self) -> bool {
        client_for_id(self.state, None).is_ok_and(|c| c.supports_labels())
    }

    fn completed_files(
        &self,
        torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        // Keep source identities visible to collection. Persisted destinations
        // deduplicate video writes there and identify the matching sidecars.
        self.client_for(torrent)?.completed_files(torrent)
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        self.client_for(torrent)?.is_claimed(torrent)
    }

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<std::path::PathBuf>, bool), DownloaderError> {
        self.client_for(torrent)?.collectable_files(torrent)
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.client_for(torrent)?.remove(torrent, delete_files)
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.client_for(torrent)?
            .remove_owned(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        self.client_for(torrent)?.owned_identity(torrent)
    }

    fn endpoint(&self) -> Option<String> {
        // Route-specific clients differ per Torrent; report the default route.
        match client_for_id(self.state, None) {
            Ok(client) => client.endpoint(),
            Err(error) => {
                tracing::error!(%error, "无法解析默认下载器端点");
                None
            }
        }
    }

    fn task_files_by_hash(
        &self,
        info_hash: &str,
    ) -> Result<Option<downloader::TaskFileSet>, DownloaderError> {
        client_for_id(self.state, None)?.task_files_by_hash(info_hash)
    }

    fn set_task_tags(&self, info_hash: &str, tags: &[String]) -> Result<(), DownloaderError> {
        client_for_id(self.state, None)?.set_task_tags(info_hash, tags)
    }

    fn set_task_category(&self, info_hash: &str, category: &str) -> Result<(), DownloaderError> {
        client_for_id(self.state, None)?.set_task_category(info_hash, category)
    }

    fn set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        client_for_id(self.state, None)?.set_task_limits(info_hash, upload_limit, download_limit)
    }

    fn disk_free_space(&self) -> Result<downloader::DiskSpaceInfo, DownloaderError> {
        client_for_id(self.state, None)?.disk_free_space()
    }
}
