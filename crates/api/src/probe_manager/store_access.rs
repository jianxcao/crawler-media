//! Shared Store access; each SQL operation leases only its database.

use std::sync::Arc;

use domain::FileCoverage;
use store::{LibraryConnection, Store, StoreError};

/// Store access for an owned capture pipeline.
#[derive(Clone)]
pub struct ProbeStoreAccess {
    pub(crate) store: Arc<Store>,
}

impl ProbeStoreAccess {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    pub(crate) async fn read_async<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> T + Send + 'static,
    ) -> Result<T, StoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|error| {
                StoreError::Io(std::io::Error::other(format!(
                    "probe reader worker: {error}"
                )))
            })
    }

    pub(crate) async fn write_async<T: Send + 'static>(
        &self,
        work: impl FnOnce(&LibraryConnection<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.library_write(work))
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
        crate::fingerprint_job::source::ensure_source_recorded(&self.store, row)?;
        crate::fingerprint_job::source::observe_source(row)
    }
}

pub(super) fn is_busy(error: &StoreError) -> bool {
    matches!(error, StoreError::WriteBusy)
        || matches!(error, StoreError::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock)
}

pub(crate) fn is_busy_message(message: &str) -> bool {
    message.contains("probe Store busy")
        || message.contains("Library Store busy")
        || message.contains("Library Registry busy")
        || message.contains("database writer is temporarily busy")
}

impl super::ProbeManager {
    pub fn store_access(&self) -> ProbeStoreAccess {
        ProbeStoreAccess::new(self.store.clone())
    }
}
