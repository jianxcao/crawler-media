//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn set_library_intro_settings(
        &self,
        id: &str,
        detect_intros: bool,
        enable_fingerprint: bool,
    ) -> Result<(), StoreError> {
        self.ensure_library_mutable(id)?;
        let tx = crate::write_tx(self.library)?;
        let disabling = tx.query_row(
            "SELECT (detect_intros=1 AND ?2=0) OR (enable_fingerprint=1 AND ?3=0)
             FROM libraries WHERE library_id=?1",
            params![id, detect_intros, enable_fingerprint],
            |row| row.get::<_, bool>(0),
        )?;
        tx.execute(
            "UPDATE libraries SET detect_intros = ?1, enable_fingerprint = ?2 WHERE library_id = ?3",
            params![detect_intros, enable_fingerprint, id],
        )?;
        if disabling {
            cancel_optional_library_work(&tx, id)?;
        }
        tx.commit()?;
        Ok(())
    }
}
