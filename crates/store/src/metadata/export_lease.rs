//! In-process single writer for an NFO path. The instance lease excludes other processes.
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use super::export::NfoExportRequest;
use crate::{Store, StoreError};

type Key = (std::path::PathBuf, String);
static ACTIVE: OnceLock<Mutex<HashSet<Key>>> = OnceLock::new();

pub(super) struct ExportLease(Key);

impl ExportLease {
    pub(super) fn acquire(store: &Store, request: &NfoExportRequest) -> Result<Self, StoreError> {
        let database = std::fs::canonicalize(store.data_dir())?;
        let path = crate::path_key::canonical_recorded_key(
            &store.library,
            &request.path.to_string_lossy(),
        );
        let key = (database, path);
        let mut active = ACTIVE
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .map_err(|_| StoreError::Invalid("NFO writer registry poisoned".into()))?;
        if !active.insert(key.clone()) {
            return Err(StoreError::Conflict(format!(
                "nfo path {} already has an active writer",
                request.path.display()
            )));
        }
        Ok(Self(key))
    }
}

impl Drop for ExportLease {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.get_or_init(|| Mutex::new(HashSet::new())).lock() {
            active.remove(&self.0);
        }
    }
}

pub(super) fn while_inactive<T>(
    store: &Store,
    path_key: &str,
    work: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let key = (
        std::fs::canonicalize(store.data_dir())?,
        path_key.to_string(),
    );
    let active = ACTIVE
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map_err(|_| StoreError::Invalid("NFO writer registry poisoned".into()))?;
    if active.contains(&key) {
        return Err(StoreError::Conflict(format!(
            "nfo path {path_key} has an active writer"
        )));
    }
    work()
}
