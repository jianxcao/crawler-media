use domain::LedgerId;
use rusqlite::OptionalExtension;

use super::types::FileDetails;
use crate::{Store, StoreError};

impl Store {
    pub fn get_file_path_and_library(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<(std::path::PathBuf, String)>, StoreError> {
        self.with_library_read(|db| db.get_file_path_and_library(file_id))
    }

    /// Probe details of a whole page of files in one query. A detail view must not read the
    /// `file_details` row per file, and it must not fall back to the filesystem either.
    pub fn file_details_batch(
        &self,
        file_ids: &[LedgerId],
    ) -> Result<std::collections::HashMap<LedgerId, FileDetails>, StoreError> {
        self.with_library_read(|db| db.file_details_batch(file_ids))
    }

    /// Whether presence reconciliation has confirmed this ledger file missing.
    pub fn ledger_is_missing(&self, file_id: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.ledger_is_missing(file_id))
    }
}

#[path = "read/connection.rs"]
mod connection;
