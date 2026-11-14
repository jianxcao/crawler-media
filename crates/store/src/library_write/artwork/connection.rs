//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn record_file_artwork(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        artwork: &FileArtwork<'_>,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let changed = record_file_artwork_in(&tx, file_id, expected, artwork)?;
        tx.commit()?;
        Ok(changed)
    }
}
