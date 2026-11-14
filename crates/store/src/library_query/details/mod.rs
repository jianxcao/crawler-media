use crate::library_query::items::{ITEM_COLUMNS, item_from_row};
use crate::library_query::types::LibraryItemDetail;
use crate::metadata::EffectiveMetadata;
use crate::metadata::locale::{LocaleFallback, select_locale};
use crate::{Store, StoreError};
use domain::{LedgerId, LibraryId, MediaId, MetadataFields, UnitKey};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

pub mod episodes;
pub mod metadata;

pub use metadata::{AdoptedFilterFacts, AdoptedPerson, library_display_locale};

pub(crate) fn fill_from_adopted(base: &mut MetadataFields, adopted: &EffectiveMetadata) {
    metadata::fill_from_adopted(base, adopted);
}

impl Store {
    /// Item detail: the real `library_items` columns, adopted metadata for the resolved
    /// locale (Scrape inputs filling the gaps), the first page of files with their coverage.
    ///
    /// Scrape inputs follow their own chain (requested → Library → instance) so a caller that
    /// asks for `en-US` reads `en-US` inputs even when no adopted metadata exists yet.
    pub fn library_item_detail(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<LibraryItemDetail>, StoreError> {
        self.with_library_read(|db| db.library_item_detail(library_id, media_id, locale))
    }

    /// Coverage of the listed files (`(-1,-1)` marks a whole-work file), in bounded
    /// file-id windows. Other files of the same Media are never decoded.
    pub fn file_units(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        files: &[domain::LibraryFile],
    ) -> Result<Vec<(LedgerId, Vec<UnitKey>)>, StoreError> {
        self.with_library_read(|db| db.file_units(library_id, media_id, files))
    }
}

#[path = "connection.rs"]
mod connection;
