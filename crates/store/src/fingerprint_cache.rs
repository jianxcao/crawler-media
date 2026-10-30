use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[cfg(test)]
#[path = "fingerprint_cache_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FingerprintCacheEntry {
    pub cache_key: String,
    pub algorithm_version: u32,
    pub sample_duration_secs: u32,
    pub media_duration_ms: Option<i64>,
    pub intro: Vec<u32>,
    pub outro: Option<Vec<u32>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaInfoCacheVersion {
    pub source_version: String,
    pub format_duration_ms: Option<i64>,
}

impl Store {
    pub fn get_fingerprint_cache(
        &self,
        ledger_id: &str,
    ) -> Result<Option<FingerprintCacheEntry>, StoreError> {
        self.with_library_read(|db| db.get_fingerprint_cache(ledger_id))
    }

    pub fn put_fingerprint_cache(
        &self,
        ledger_id: &str,
        cache: &FingerprintCacheEntry,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.put_fingerprint_cache(ledger_id, cache))
    }

    /// Persist a cache captured from `expected`. `None` keeps the unchecked write
    /// for callers that have no pinned version; a pinned version that no longer
    /// matches is refused and writes nothing.
    pub fn put_fingerprint_cache_checked(
        &self,
        ledger_id: &str,
        expected: Option<&domain::FileVersion>,
        cache: &FingerprintCacheEntry,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.put_fingerprint_cache_checked(ledger_id, expected, cache))
    }

    pub fn get_media_info_cache_version(
        &self,
        ledger_id: &str,
    ) -> Result<Option<MediaInfoCacheVersion>, StoreError> {
        self.with_library_read(|db| db.get_media_info_cache_version(ledger_id))
    }

    pub fn delete_fingerprint_cache(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.with_library_write(|db| db.delete_fingerprint_cache(ledger_id))
    }
}

#[path = "fingerprint_cache/connection.rs"]
mod connection;
