//! Scoped read freshness for observations taken outside the Store lock.
//!
//! An observation captures [`ReadFingerprint`] for the exact `(library, media)`
//! scope it read; admitting a side effect re-reads the same fingerprint and
//! rejects the round when anything changed. Only tables that still exist are
//! read, and a storage error propagates instead of degrading into a value that
//! can never mismatch.

use domain::{MediaId, UnitKey};
use rusqlite::params;

use crate::{Store, StoreError};

/// One file's contribution to a media-scope fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReadRevision {
    pub file_id: String,
    pub file_revision: i64,
    pub presence_state: String,
    pub publication_state: String,
    pub root_id: String,
    pub path: String,
    pub source_version: String,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub hdr_observed: bool,
    pub quality_source: String,
    pub confidence: String,
    pub filter_score: Option<i32>,
    pub release_quality: Option<String>,
    pub units: Vec<UnitKey>,
}

/// Everything one Library observation depends on: the Library's configuration
/// revision plus the ordered file facts of one media.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadFingerprint {
    pub library_id: String,
    pub media_id: MediaId,
    pub config_revision: i64,
    /// Ordered by `file_id` so two captures of the same facts compare equal.
    pub files: Vec<FileReadRevision>,
}

impl Store {
    /// Capture the fingerprint of the facts an observation of `media_id` in
    /// `library_id` depends on. A missing Library is an error, not revision 0.
    pub fn read_fingerprint(
        &self,
        library_id: &str,
        media_id: MediaId,
    ) -> Result<ReadFingerprint, StoreError> {
        let config_revision = self.library_config_revision(library_id)?;
        let mut stmt = self.library.prepare(
            "SELECT f.file_id, f.file_revision, f.presence_state, f.publication_state,
                    f.root_id, f.display_path, f.source_version, f.resolution, f.codec,
                    f.hdr, f.quality_source, f.confidence, f.filter_score, f.release_quality,
                    f.hdr_observed,
                    u.season, u.episode
             FROM library_files f
             LEFT JOIN library_file_units u ON u.file_id = f.file_id
             WHERE f.library_id = ?1 AND f.media_id = ?2
             ORDER BY f.file_id, u.season, u.episode",
        )?;
        let mut rows = stmt.query(params![library_id, media_id.to_string()])?;
        let mut files: Vec<FileReadRevision> = Vec::new();
        while let Some(row) = rows.next()? {
            let file_id: String = row.get(0)?;
            if files.last().is_none_or(|file| file.file_id != file_id) {
                files.push(FileReadRevision {
                    file_id,
                    file_revision: row.get(1)?,
                    presence_state: row.get(2)?,
                    publication_state: row.get(3)?,
                    root_id: row.get(4)?,
                    path: row.get(5)?,
                    source_version: row.get(6)?,
                    resolution: row.get(7)?,
                    codec: row.get(8)?,
                    hdr: row.get(9)?,
                    quality_source: row.get(10)?,
                    confidence: row.get(11)?,
                    filter_score: row.get(12)?,
                    release_quality: row.get(13)?,
                    hdr_observed: row.get(14)?,
                    units: Vec::new(),
                });
            }
            if let (Some(season), Some(episode)) = (row.get(15)?, row.get(16)?)
                && let Some(file) = files.last_mut()
            {
                file.units.push(UnitKey::new(season, episode));
            }
        }
        tracing::debug!(
            library_id,
            media_id = %media_id,
            config_revision,
            file_count = files.len(),
            "captured scoped read fingerprint"
        );
        Ok(ReadFingerprint {
            library_id: library_id.to_string(),
            media_id,
            config_revision,
            files,
        })
    }
}
