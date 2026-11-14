//! Clear Subscribe targets after their Library finalization has committed.

use domain::LibraryId;

use crate::{Store, StoreError};

impl Store {
    /// Leave Subscribes using the same-kind default after their target is removed.
    /// Call after committing library.db; this transaction owns only subscribe.db.
    pub fn detach_subscribes_from_library(&self, library: LibraryId) -> Result<usize, StoreError> {
        self.with_subscribe_write(|db| db.detach_subscribes_from_library(library))
    }
}

#[path = "subscribe_detach/connection.rs"]
mod connection;
