//! Scoped visible media batch resolution.

use domain::{LibraryId, MediaId, UserId};
use store::{Store, StoreError};

pub struct VisibleScope {
    pub media_ids: Vec<MediaId>,
}

impl VisibleScope {
    /// Distinct Media with an item in a Library visible to `user_id`, read in SQL.
    pub fn load(store: &Store, user_id: UserId) -> Result<Self, StoreError> {
        let visible: Vec<LibraryId> = store
            .list_libraries()?
            .iter()
            .filter(|lib| crate::http::library::library_visible(store, lib, Some(user_id)))
            .filter_map(|lib| lib.id.parse().ok())
            .collect();
        Ok(Self {
            media_ids: store.media_ids_in_libraries(&visible)?,
        })
    }

    pub fn media_ids(&self) -> &[MediaId] {
        &self.media_ids
    }
}
