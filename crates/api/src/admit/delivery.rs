//! Freeze manual delivery configuration under SQL lock.
use crate::runtime_downloader::{ChosenDownloader, DownloaderEnv, row_to_chosen};
use crate::{ApiState, management::ApiError};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{DownloaderId, Subscribe, SubscribeId, Torrent};

pub(super) struct Delivery {
    pub id: Option<DownloaderId>,
}

pub(crate) fn ensure_subscribe_library_available(
    state: &ApiState,
    sub: &Subscribe,
) -> Result<(), Response> {
    let lib_id = match sub.library_id.as_ref() {
        Some(id) => id.to_string(),
        None => {
            let media = match state.store.get_media(sub.media_id) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    return Err(
                        ApiError::missing("media.missing", "媒体不存在".into()).into_response()
                    );
                }
                Err(
                    store::StoreError::WriteBusy
                    | store::StoreError::ReadBusy
                    | store::StoreError::ReadDeadline,
                ) => return Err(crate::http::store_wait::busy_response()),
                Err(err) => return Err(ApiError::from(err).into_response()),
            };
            match state.store.default_library(media.kind) {
                Ok(Some(lib)) => lib.id,
                Ok(None) => {
                    return Err(ApiError::with(
                        StatusCode::CONFLICT,
                        "library.unavailable",
                        "目标媒体库不可写入".into(),
                    )
                    .into_response());
                }
                Err(
                    store::StoreError::WriteBusy
                    | store::StoreError::ReadBusy
                    | store::StoreError::ReadDeadline,
                ) => return Err(crate::http::store_wait::busy_response()),
                Err(err) => return Err(ApiError::from(err).into_response()),
            }
        }
    };
    let accepts = match state.store.library_accepts_work(&lib_id) {
        Ok(ok) => ok,
        Err(
            store::StoreError::WriteBusy
            | store::StoreError::ReadBusy
            | store::StoreError::ReadDeadline,
        ) => return Err(crate::http::store_wait::busy_response()),
        Err(err) => return Err(ApiError::from(err).into_response()),
    };
    if state.library_work().is_cancelled(&lib_id) || !accepts {
        return Err(ApiError::with(
            StatusCode::CONFLICT,
            "library.unavailable",
            "目标媒体库不可写入".into(),
        )
        .into_response());
    }
    Ok(())
}

pub(super) struct ManualAdmitGuard {
    inflight: std::sync::Arc<parking_lot::Mutex<std::collections::HashSet<(SubscribeId, String)>>>,
    key: (SubscribeId, String),
}

impl ManualAdmitGuard {
    pub(super) fn acquire(
        state: &ApiState,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<Self, Response> {
        let key = (subscribe_id, enclosure.to_string());
        if !state.manual_admit_inflight.lock().insert(key.clone()) {
            return Err(ApiError::with(
                StatusCode::CONFLICT,
                "subscription.admitting",
                "正在提交该资源，请稍后重试".into(),
            )
            .into_response());
        }
        Ok(Self {
            inflight: state.manual_admit_inflight.clone(),
            key,
        })
    }
}

impl Drop for ManualAdmitGuard {
    fn drop(&mut self) {
        self.inflight.lock().remove(&self.key);
    }
}

pub(super) fn prepare(
    state: &ApiState,
    subscribe: &Subscribe,
    torrent: &Torrent,
) -> Result<Delivery, Response> {
    let (id, row) = super::admitted_store(state, |store| {
        let id = crate::delivery::target_id_from_store(store, Some(subscribe), torrent)
            .map_err(error)?;
        let row = match id {
            Some(id) if !store.default_downloader()?.is_some_and(|row| row.id == id) => {
                store.get_downloader(id)?
            }
            _ => None,
        };
        Ok((id, row))
    })?;
    if let Some(row) = row {
        match row_to_chosen(row, &DownloaderEnv::default()) {
            ChosenDownloader::Qbittorrent(_) | ChosenDownloader::Transmission(_) => {}
            _ => return Err(error("Downloader is incomplete").into_response()),
        }
    }
    Ok(Delivery { id })
}

fn error(error: impl ToString) -> ApiError {
    tracing::error!(error=%error.to_string(), "Manual Torrent delivery failed");
    ApiError::invalid("subscribe.invalid", error.to_string())
}
