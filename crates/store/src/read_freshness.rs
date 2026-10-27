//! Scoped read freshness for observations taken outside the Store lock.
//!
//! An observation captures [`ReadFingerprint`] for the exact `(library, media)`
//! scope it read; admitting a side effect re-reads the same fingerprint and
//! rejects the round when anything changed. Only tables that still exist are
//! read, and a storage error propagates instead of degrading into a value that
//! can never mismatch.

use domain::MediaId;
use rusqlite::params;

use crate::{Store, StoreError};

/// One file's contribution to a media-scope fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReadRevision {
    pub file_id: String,
    pub file_revision: i64,
    pub presence_state: String,
    pub publication_state: String,
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
            "SELECT file_id, file_revision, presence_state, publication_state
             FROM library_files
             WHERE library_id = ?1 AND media_id = ?2
             ORDER BY file_id",
        )?;
        let rows = stmt.query_map(params![library_id, media_id.to_string()], |row| {
            Ok(FileReadRevision {
                file_id: row.get(0)?,
                file_revision: row.get(1)?,
                presence_state: row.get(2)?,
                publication_state: row.get(3)?,
            })
        })?;
        let files = rows.collect::<Result<Vec<_>, _>>()?;
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
