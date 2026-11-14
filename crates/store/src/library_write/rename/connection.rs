//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub(super) fn rename_within_root(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        resolved_to: &Path,
        resolved_libraries: &[crate::Library],
    ) -> Result<(), StoreError> {
        let tx = crate::read_tx(self.library)?;
        let id = file_id.to_string();
        let from_str = from.to_string_lossy().into_owned();
        let owned = load_owned(&tx, &id, &from_str)?;
        if owned.revision != expected.revision || owned.source_version != expected.source_version {
            return Err(StoreError::Protected(format!("file {id} version changed")));
        }
        ensure_library_writable(&tx, &owned.library_id)?;
        let media_id: MediaId = owned.media_id.parse()?;
        ensure_media_writable(&tx, media_id)?;
        self.ensure_same_owner(&tx, &owned, to, resolved_to, resolved_libraries)?;
        let to_str = to.to_string_lossy().into_owned();
        let occupied: i64 = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files
                           WHERE path_key = norm_path_key(?1) AND file_id <> ?2)",
            params![to_str, id],
            |r| r.get(0),
        )?;
        if occupied != 0 {
            return Err(StoreError::Protected(format!(
                "{to_str} is already recorded"
            )));
        }
        drop(tx);
        Ok(())
    }

    pub(super) fn ensure_same_owner(
        &self,
        tx: &rusqlite::Connection,
        owned: &Owned,
        to: &Path,
        resolved_to: &Path,
        resolved_libraries: &[crate::Library],
    ) -> Result<(), StoreError> {
        let root: String = tx
            .query_row(
                "SELECT display_path FROM library_roots WHERE root_id = ?1 AND library_id = ?2",
                params![owned.root_id, owned.library_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid(format!("root {} missing", owned.root_id)))?;
        let outside = || {
            StoreError::Protected(format!(
                "{} is outside the file's own root/Library; re-parenting needs a durable operation",
                to.display()
            ))
        };
        if !to.is_absolute() {
            return Err(outside());
        }
        let canonical_root = self.recorded_path_key(&root);
        if !resolved_to.starts_with(Path::new(&canonical_root)) {
            return Err(outside());
        }
        let kind: MediaKind = owned
            .kind
            .parse()
            .map_err(|_| StoreError::Invalid(format!("kind {}", owned.kind)))?;
        match crate::library_ownership::classify_new_file(resolved_libraries, resolved_to, kind) {
            Some(library) if library.id == owned.library_id => Ok(()),
            _ => Err(outside()),
        }
    }
}
