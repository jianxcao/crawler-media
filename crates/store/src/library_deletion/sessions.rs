use crate::{Store, StoreError};

impl Store {
    /// Close one indexed window by its recorded physical source. `file_id` is
    /// required by the current schema: Media/coordinate guesses are never needed.
    pub fn close_playback_sessions_for_removal_batch(&self, id: &str) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.close_playback_sessions_for_removal_batch(id))
    }

    pub fn close_playback_sessions_for_removal(&self, id: &str) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.close_playback_sessions_for_removal(id))
    }
}

#[path = "sessions/connection.rs"]
mod connection;
