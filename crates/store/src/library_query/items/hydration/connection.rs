//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_item_facts(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        user_id: Option<UserId>,
    ) -> Result<HashMap<MediaId, LibraryItemFacts>, StoreError> {
        library_item_facts_on(self.library, library_id, media_ids, user_id)
    }

    pub fn library_item_stills(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
    ) -> Result<Vec<LibraryStill>, StoreError> {
        library_item_stills_on(self.library, library_id, media_ids)
    }
}
