use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

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
        let raw = self
            .library
            .query_row(
                "SELECT cache_key, algorithm_version, sample_duration_secs,
                        media_duration_ms, intro_json, outro_json
                 FROM fingerprint_cache WHERE file_id = ?1",
                params![ledger_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        raw.map(
            |(
                cache_key,
                algorithm_version,
                sample_duration_secs,
                media_duration_ms,
                intro,
                outro,
            )| {
                Ok(FingerprintCacheEntry {
                    cache_key,
                    algorithm_version,
                    sample_duration_secs,
                    media_duration_ms,
                    intro: serde_json::from_str(&intro)?,
                    outro: outro
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                })
            },
        )
        .transpose()
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
        if let Some(expected) = expected {
            let current: Option<(i64, String)> = self
                .library
                .query_row(
                    "SELECT file_revision, source_version FROM library_files WHERE file_id = ?1",
                    [ledger_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((revision, source_version)) = current else {
                return Err(StoreError::Missing(format!("file_deleted: {ledger_id}")));
            };
            if revision != expected.revision || source_version != expected.source_version {
                return Err(StoreError::Protected(format!(
                    "fingerprint source changed: {ledger_id}"
                )));
            }
        }
        self.library.execute(
            "INSERT INTO fingerprint_cache (
                 file_id, cache_key, algorithm_version, sample_duration_secs,
                 media_duration_ms, intro_json, outro_json, captured_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(file_id) DO UPDATE SET
               cache_key = excluded.cache_key,
               algorithm_version = excluded.algorithm_version,
               sample_duration_secs = excluded.sample_duration_secs,
               media_duration_ms = excluded.media_duration_ms,
               intro_json = excluded.intro_json,
               outro_json = excluded.outro_json,
               captured_at_ms = excluded.captured_at_ms",
            params![
                ledger_id,
                cache.cache_key,
                cache.algorithm_version,
                cache.sample_duration_secs,
                cache.media_duration_ms,
                serde_json::to_string(&cache.intro)?,
                cache
                    .outro
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                super::unix_now() * 1000,
            ],
        )?;
        Ok(())
    }

    pub fn get_media_info_cache_version(
        &self,
        ledger_id: &str,
    ) -> Result<Option<MediaInfoCacheVersion>, StoreError> {
        self.library
            .query_row(
                "SELECT source_version, format_duration_ms FROM file_details
                 WHERE file_id = ?1 AND tracks_version >= ?2 AND video_json IS NOT NULL
                   AND source_version IS NOT NULL",
                params![ledger_id, super::library::CURRENT_TRACKS_VERSION],
                |row| {
                    Ok(MediaInfoCacheVersion {
                        source_version: row.get(0)?,
                        format_duration_ms: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_fingerprint_cache(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM fingerprint_cache WHERE file_id = ?1",
            params![ledger_id],
        )?;
        Ok(())
    }
}
