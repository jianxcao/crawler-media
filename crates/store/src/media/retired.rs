use domain::MediaId;

use crate::{Store, StoreError};

impl Store {
    pub fn retired_media_survivors(&self) -> Result<Vec<(MediaId, MediaId)>, StoreError> {
        self.with_library_read(|db| db.retired_media_survivors())
    }
}

#[path = "retired/connection.rs"]
mod connection;
