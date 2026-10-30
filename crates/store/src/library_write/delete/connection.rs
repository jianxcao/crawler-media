use super::*;

impl crate::LibraryConnection<'_> {
    pub fn delete_files_with_references(
        &self,
        subscribes: &crate::SubscribeConnection<'_>,
        library_id: LibraryId,
        file_ids: &[LedgerId],
    ) -> Result<usize, StoreError> {
        if file_ids.is_empty() {
            return Ok(0);
        }
        let tx = crate::write_tx(self.library)?;
        let (removed, affected) = delete_files_in_tx(&tx, library_id, file_ids)?;
        let subscribed = subscribes.subscribed_media(&affected)?;
        super::super::orphan_media::purge_orphan_media_in_tx(&tx, &affected, &subscribed)?;
        tx.commit()?;
        Ok(removed)
    }
}
