use crate::StoreError;

use super::{LibraryReconfiguration, root_keys};

/// Owned filesystem evidence for one exact request. Build this without Store;
/// committing it still validates lifecycle and the original configuration revision.
pub struct PreparedLibraryReconfiguration {
    pub(super) request: LibraryReconfiguration,
    pub(super) roots: Option<Vec<root_keys::PreparedRoot>>,
}

impl LibraryReconfiguration {
    pub fn prepare(&self) -> Result<PreparedLibraryReconfiguration, StoreError> {
        if self.root_paths.is_none() && self.display_locale.is_none() {
            return Err(StoreError::Invalid(
                "library reconfiguration needs a root set or a locale".into(),
            ));
        }
        if self.root_paths.as_ref().is_some_and(Vec::is_empty) {
            return Err(StoreError::Invalid(
                "library reconfiguration cannot drop every root".into(),
            ));
        }
        if self
            .display_locale
            .as_deref()
            .is_some_and(|locale| locale.trim().is_empty())
        {
            return Err(StoreError::Invalid("display locale cannot be empty".into()));
        }
        Ok(PreparedLibraryReconfiguration {
            request: self.clone(),
            roots: self
                .root_paths
                .as_deref()
                .map(root_keys::prepare_roots)
                .transpose()?,
        })
    }
}
