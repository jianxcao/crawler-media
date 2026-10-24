//! Library ownership from already-registered root keys. No filesystem IO.

use rusqlite::params;

use crate::{Store, StoreError};
use domain::MediaKind;

impl Store {
    /// Library that owns `path` by its registered root keys.
    ///
    /// The caller resolves `path` before taking the Store lock; this only compares keys.
    pub fn library_owning_key(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<String>, StoreError> {
        let key = crate::path_key::canonical_key(
            &crate::path_key::canonical_existing_key(&path.to_string_lossy()),
            &crate::path_key::alias_snapshot(&self.library),
        );
        let mut stmt = self.library.prepare(
            "SELECT r.path_key, l.library_id FROM library_roots r
             JOIN libraries l ON l.library_id = r.library_id
             WHERE l.kind = ?1",
        )?;
        let rows = stmt.query_map(params![kind.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut best: Option<(usize, String)> = None;
        for row in rows {
            let (root, library_id) = row?;
            if key != root && !key.starts_with(&(root.clone() + "/")) {
                continue;
            }
            let depth = std::path::Path::new(&root).components().count();
            if best.as_ref().is_none_or(|(best_depth, _)| depth > *best_depth) {
                best = Some((depth, library_id));
            }
        }
        Ok(best.map(|(_, id)| id))
    }
}
