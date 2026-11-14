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
    pub intro: marker::CapturedFingerprint,
    pub outro: Option<marker::CapturedFingerprint>,
}

/// Owned columns from one snapshot; decode only after the enclosing read stage.
pub struct FingerprintCacheRead {
    cache_key: String,
    algorithm_version: u32,
    sample_duration_secs: u32,
    media_duration_ms: Option<i64>,
    intro_json: String,
    outro_json: Option<String>,
}

/// Immutable JSON prepared before capture publication obtains its writer.
pub struct PreparedFingerprintCache(FingerprintCacheRead);

impl FingerprintCacheEntry {
    pub fn prepare(&self) -> Result<PreparedFingerprintCache, StoreError> {
        Ok(PreparedFingerprintCache(FingerprintCacheRead {
            cache_key: self.cache_key.clone(),
            algorithm_version: self.algorithm_version,
            sample_duration_secs: self.sample_duration_secs,
            media_duration_ms: self.media_duration_ms,
            intro_json: serde_json::to_string(&self.intro)?,
            outro_json: self.outro.as_ref().map(serde_json::to_string).transpose()?,
        }))
    }
}

impl FingerprintCacheRead {
    pub fn decode(self) -> Result<FingerprintCacheEntry, StoreError> {
        Ok(FingerprintCacheEntry {
            cache_key: self.cache_key,
            algorithm_version: self.algorithm_version,
            sample_duration_secs: self.sample_duration_secs,
            media_duration_ms: self.media_duration_ms,
            intro: serde_json::from_str(&self.intro_json)?,
            outro: self
                .outro_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        })
    }
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
        self.read_fingerprint_cache(ledger_id)?
            .map(FingerprintCacheRead::decode)
            .transpose()
    }

    pub fn read_fingerprint_cache(
        &self,
        ledger_id: &str,
    ) -> Result<Option<FingerprintCacheRead>, StoreError> {
        self.with_library_read(|db| db.read_fingerprint_cache(ledger_id))
    }

    pub fn put_fingerprint_cache(
        &self,
        ledger_id: &str,
        cache: &FingerprintCacheEntry,
    ) -> Result<(), StoreError> {
        self.put_fingerprint_cache_checked(ledger_id, None, cache)
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
        let prepared = cache.prepare()?;
        self.with_library_write(|db| {
            db.put_fingerprint_cache_checked(ledger_id, expected, &prepared)
        })
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
