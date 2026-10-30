//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_owning_key(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .library_root_owning_key(path, kind)?
            .map(|(library, _)| library))
    }

    pub fn library_root_owning_key(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<(String, String)>, StoreError> {
        owning_root_key(self.library, path, kind)
    }
}
