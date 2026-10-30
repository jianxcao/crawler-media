//! Bounded coverage windows by explicit recorded ownership.
use super::LedgerPresenceObservation;
use crate::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit};
use crate::{Store, StoreError};
use domain::{LedgerId, LibraryId, MediaId};
use rusqlite::params;

impl Store {
    /// At most 100 files, with complete coverage and missing flags captured together.
    /// The cursor is exclusive and refers to file identity, never joined unit rows.
    pub fn ledger_presence_page(
        &self,
        library_id: LibraryId,
        media_id: Option<MediaId>,
        after: Option<LedgerId>,
        limit: usize,
    ) -> Result<Vec<LedgerPresenceObservation>, StoreError> {
        self.with_library_read(|db| db.ledger_presence_page(library_id, media_id, after, limit))
    }

    /// Same production read plus SQLite VM and full-scan steps for workload assertions.
    pub fn ledger_presence_page_with_work(
        &self,
        library_id: LibraryId,
        media_id: Option<MediaId>,
        after: Option<LedgerId>,
        limit: usize,
    ) -> Result<(Vec<LedgerPresenceObservation>, i64, i64), StoreError> {
        self.with_library_read(|db| {
            db.ledger_presence_page_with_work(library_id, media_id, after, limit)
        })
    }
}

#[path = "read/connection.rs"]
mod connection;
