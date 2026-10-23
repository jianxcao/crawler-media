//! Scoped visible media batch resolution.

use domain::{LibraryId, MediaId, UserId};
use store::{Store, StoreError};

pub struct VisibleScope {
    pub media_ids: Vec<MediaId>,
}

impl VisibleScope {
    /// Distinct Media with an item in a Library visible to `user_id`, read in SQL.
    pub fn load(store: &Store, user_id: UserId) -> Result<Self, StoreError> {
        let visible = visible_library_ids(store, user_id)?;
        Ok(Self {
            media_ids: store.media_ids_in_libraries(&visible)?,
        })
    }

    pub fn media_ids(&self) -> &[MediaId] {
        &self.media_ids
    }
}

/// Libraries `user_id` may read, by persisted id.
pub fn visible_library_ids(store: &Store, user_id: UserId) -> Result<Vec<LibraryId>, StoreError> {
    Ok(store
        .list_libraries()?
        .iter()
        .filter(|lib| crate::http::library::library_visible(store, lib, Some(user_id)))
        .filter_map(|lib| lib.id.parse().ok())
        .collect())
}

/// Libraries a user may read, keyed by persisted id, loaded once per request.
pub struct VisibleLibraries {
    libraries: std::collections::HashMap<LibraryId, store::Library>,
}

impl VisibleLibraries {
    pub fn load(store: &Store, user_id: Option<UserId>) -> Result<Self, StoreError> {
        let libraries = store
            .list_libraries()?
            .into_iter()
            .filter(|lib| crate::http::library::library_visible(store, lib, user_id))
            .filter_map(|lib| Some((lib.id.parse().ok()?, lib)))
            .collect();
        Ok(Self { libraries })
    }

    pub fn ids(&self) -> Vec<LibraryId> {
        let mut ids: Vec<LibraryId> = self.libraries.keys().copied().collect();
        ids.sort_by_key(LibraryId::to_string);
        ids
    }

    pub fn get(&self, id: &LibraryId) -> Option<&store::Library> {
        self.libraries.get(id)
    }

    /// Published files of `media` owned (persisted `library_id`) by a visible library
    /// that `keep` accepts. One indexed read by Media, never a whole-ledger scan.
    pub fn media_coverage(
        &self,
        store: &Store,
        media: MediaId,
        keep: impl Fn(&store::Library) -> bool,
    ) -> Result<Vec<domain::FileCoverage>, StoreError> {
        Ok(store
            .file_coverage_for_media(media)?
            .into_iter()
            .filter(|cov| cov.file.publication == domain::FilePublication::Published)
            .filter(|cov| self.get(&cov.file.library_id).is_some_and(&keep))
            .collect())
    }
}
