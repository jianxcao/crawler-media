//! Shared reader leases and bounded publication writes for probe work.

use std::sync::Arc;

use domain::FileCoverage;
use parking_lot::{Mutex, MutexGuard};
use store::{Store, StoreError};

use crate::http::store_wait::{STORE_READ_WAIT, STORE_WRITE_WAIT};
use crate::store_readers::{StoreReader, StoreReaders};

/// Store access for a capture pipeline, sharing its manager's reader pool.
#[derive(Clone)]
pub struct ProbeStoreAccess {
    writer: Arc<Mutex<Store>>,
    readers: Arc<StoreReaders>,
}

impl ProbeStoreAccess {
    /// Construct an independent pipeline with its own bounded read pool.
    pub fn new(writer: Arc<Mutex<Store>>) -> Result<Self, StoreError> {
        let readers = Arc::new(StoreReaders::open(
            &writer.lock(),
            crate::store_readers::STORE_READERS,
        )?);
        Ok(Self::shared(writer, readers))
    }

    pub(crate) fn shared(writer: Arc<Mutex<Store>>, readers: Arc<StoreReaders>) -> Self {
        Self { writer, readers }
    }

    pub(crate) fn reader(&self) -> Result<StoreReader, StoreError> {
        self.readers.checkout(STORE_READ_WAIT).ok_or_else(|| {
            tracing::warn!("探测读池等待预算耗尽，保留任务等待重试");
            busy_error()
        })
    }

    pub(crate) fn write(&self) -> Result<MutexGuard<'_, Store>, StoreError> {
        self.writer.try_lock_for(STORE_WRITE_WAIT).ok_or_else(|| {
            tracing::warn!("探测写入等待预算耗尽，保留任务等待重试");
            busy_error()
        })
    }

    pub(crate) async fn read_async<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> T + Send + 'static,
    ) -> Result<T, StoreError> {
        let reader = self
            .readers
            .checkout_async(STORE_READ_WAIT)
            .await
            .ok_or_else(busy_error)?;
        tokio::task::spawn_blocking(move || work(&reader))
            .await
            .map_err(|error| {
                StoreError::Io(std::io::Error::other(format!(
                    "probe reader worker: {error}"
                )))
            })
    }

    pub(crate) async fn write_async<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let access = self.clone();
        tokio::task::spawn_blocking(move || {
            let store = access.write()?;
            work(&store)
        })
        .await
        .map_err(|error| {
            StoreError::Io(std::io::Error::other(format!(
                "probe writer worker: {error}"
            )))
        })?
    }

    pub(crate) async fn source_available_async(&self, row: &FileCoverage) -> Result<(), String> {
        let access = self.clone();
        let row = row.clone();
        tokio::task::spawn_blocking(move || access.source_available(&row))
            .await
            .map_err(|error| format!("probe source worker: {error}"))?
    }

    pub(crate) fn source_available(&self, row: &FileCoverage) -> Result<(), String> {
        {
            let reader = self.reader().map_err(|error| error.to_string())?;
            crate::fingerprint_job::source::ensure_source_recorded(&reader, row)?;
        }
        crate::fingerprint_job::source::observe_source(row)
    }
}

pub(super) fn busy_error() -> StoreError {
    StoreError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "probe Store busy",
    ))
}

pub(super) fn is_busy(error: &StoreError) -> bool {
    matches!(error, StoreError::WriteBusy)
        || matches!(error, StoreError::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock)
}

pub(crate) fn is_busy_message(message: &str) -> bool {
    message.contains("probe Store busy")
        || message.contains("Library Store busy")
        || message.contains("Library Registry busy")
        || message.contains("app writer is temporarily busy")
}

impl super::ProbeManager {
    /// Share this manager's reader pool and bounded writer with a capture pipeline.
    pub fn store_access(&self) -> ProbeStoreAccess {
        ProbeStoreAccess::shared(self.store.clone(), self.readers.clone())
    }

    pub(super) fn reader(&self) -> Result<StoreReader, StoreError> {
        self.store_access().reader()
    }

    pub(super) fn writer(&self) -> Result<MutexGuard<'_, Store>, StoreError> {
        self.dispatch_store().ok_or_else(busy_error)
    }

    pub(super) fn dispatch_reader(&self) -> Option<StoreReader> {
        self.reader().ok()
    }
}
