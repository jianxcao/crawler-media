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
        self.with_library_read(|db| db.read_fingerprint(library_id, media_id))
    }
}

#[path = "read_freshness/connection.rs"]
mod connection;
