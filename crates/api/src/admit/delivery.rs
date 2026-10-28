//! Freeze manual delivery configuration under SQL lock; resolve/connect outside it.
use crate::runtime_downloader::{ChosenDownloader, DownloaderEnv, row_to_chosen};
use crate::{ApiState, management::ApiError};
use axum::response::{IntoResponse, Response};
use domain::{DownloaderId, Subscribe, Torrent};
use downloader::{Downloader, QbitDownloader, TransmissionDownloader};
use std::sync::Arc;

pub(super) struct Delivery {
    pub id: Option<DownloaderId>,
    client: Arc<dyn Downloader>,
    site: Option<domain::Site>,
}

pub(super) fn prepare(
    state: &ApiState,
    subscribe: &Subscribe,
    torrent: &Torrent,
) -> Result<Delivery, Response> {
    let (id, row, site) = super::admitted_store(state, |store| {
        let id = crate::delivery::target_id_from_store(store, Some(subscribe), torrent)
            .map_err(error)?;
        let row = match id {
            Some(id) if !store.default_downloader()?.is_some_and(|row| row.id == id) => {
                store.get_downloader(id)?
            }
            _ => None,
        };
        Ok((id, row, store.get_site(torrent.site_id)?))
    })?;
    let client: Arc<dyn Downloader> = match row {
        None => state.downloader.clone(),
        Some(row) => match row_to_chosen(row, &DownloaderEnv::default()) {
            ChosenDownloader::Qbittorrent(config) => {
                Arc::new(QbitDownloader::connect(config).map_err(|e| error(e).into_response())?)
            }
            ChosenDownloader::Transmission(config) => Arc::new(
                TransmissionDownloader::connect(config).map_err(|e| error(e).into_response())?,
            ),
            _ => return Err(error("Downloader is incomplete").into_response()),
        },
    };
    let client = client
        .snapshot_client()
        .map_err(|e| error(e).into_response())?
        .unwrap_or(client);
    Ok(Delivery { id, client, site })
}

impl Delivery {
    pub fn resolve_url(&self, state: &ApiState, torrent: &Torrent) -> Result<String, Response> {
        self.site
            .as_ref()
            .map(|site| state.indexer.resolve_torrent_download(site, torrent))
            .transpose()
            .map_err(|e| error(e).into_response())
            .map(|url| url.flatten().unwrap_or_else(|| torrent.enclosure.clone()))
    }

    pub fn submit(&self, torrent: &Torrent, url: &str) -> Result<(), Response> {
        self.client
            .add_with_options(torrent, url, None)
            .map_err(|e| error(e).into_response())
    }
}

fn error(error: impl ToString) -> ApiError {
    tracing::error!(error=%error.to_string(), "Manual Torrent delivery failed");
    ApiError::invalid("subscribe.invalid", error.to_string())
}
