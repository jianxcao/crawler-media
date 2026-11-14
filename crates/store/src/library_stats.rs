use super::{Store, StoreError};
use rusqlite::params;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryRosterStats {
    pub item_count: u64,
    pub file_count: u64,
    pub total_size_bytes: u64,
    pub strm_file_count: u64,
}

impl Store {
    pub fn library_roster_stats(&self, library_id: &str) -> Result<LibraryRosterStats, StoreError> {
        self.with_library_read(|db| db.library_roster_stats(library_id))
    }
}

#[path = "library_stats/connection.rs"]
mod connection;
